package io.github.muntashirakon.adb;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import org.junit.Test;

import java.io.IOException;
import java.io.FilterOutputStream;
import java.io.OutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.security.KeyPairGenerator;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.BooleanSupplier;
import java.util.concurrent.ConcurrentHashMap;

public class AdbStreamCompletionTest {
    @Test(timeout = 15_000)
    public void acknowledgedStreamClosedByPeerAfterOpenReturnsEndsAtEof() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer()) {
            peer.start();
            java.security.KeyPair keyPair = generatedKeyPair();
            try (AdbConnection connection = AdbConnection.create("127.0.0.1", peer.getPort(),
                    new KeyPair(keyPair.getPrivate(), new TestCertificate(keyPair.getPublic())), 1)) {
                connection.connect();
                AdbStream stream = connection.open("shell:cmd appops set example READ_RESTRICTED_MESSAGES allow");
                peer.closeStream();
                assertEquals(-1, stream.openInputStream().read());
            }
        }
    }

    @Test(timeout = 15_000)
    public void readDrainsQueuedPayloadThenRemainsAtEof() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer()) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                AdbStream stream = connection.open("shell:echo payload");
                peer.sendPayloadAndClose(new byte[]{1, 2, 3});
                byte[] bytes = new byte[4];
                assertEquals(3, stream.read(bytes, 0, bytes.length));
                assertEquals(1, bytes[0]);
                assertEquals(2, bytes[1]);
                assertEquals(3, bytes[2]);
                assertEquals(-1, stream.read(bytes, 0, bytes.length));
                assertEquals(-1, stream.read(bytes, 0, bytes.length));
            }
        }
    }

    @Test(timeout = 15_000)
    public void readChecksBoundsAndReturnsZeroWithoutWaitingAtTerminal() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer()) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                AdbStream stream = connection.open("shell:true");
                byte[] bytes = new byte[2];
                assertBoundsFailure(() -> stream.read(bytes, -1, 1));
                assertBoundsFailure(() -> stream.read(bytes, 1, 2));
                assertBoundsFailure(() -> stream.read(bytes, 0, -1));
                assertEquals(0, stream.read(bytes, 2, 0));
                peer.closeStream();
                assertEquals(0, stream.read(bytes, 2, 0));
                assertEquals(-1, stream.read(bytes, 0, 1));
            }
        }
    }

    @Test(timeout = 15_000)
    public void interruptedReadRecordsFailureThatWinsOverPeerClose() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer()) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                AdbStream stream = connection.open("shell:wait");
                AtomicReference<Throwable> result = new AtomicReference<>();
                CountDownLatch readerStarted = new CountDownLatch(1);
                Thread reader = new Thread(() -> {
                    try {
                        readerStarted.countDown();
                        stream.read(new byte[1], 0, 1);
                    } catch (Throwable throwable) {
                        result.set(throwable);
                    }
                });
                reader.start();
                assertTrue(readerStarted.await(1, TimeUnit.SECONDS));
                reader.interrupt();
                reader.join(1_000);
                assertFalse(reader.isAlive());
                assertTrue(result.get() instanceof IOException);
                peer.closeStream();
                try {
                    stream.write(new byte[]{1}, 0, 1);
                    fail("write after interrupted read should fail");
                } catch (IOException expected) {
                    assertEquals(result.get(), expected);
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void rejectedCloseWithoutOkayRejectsOpen() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OpenAttempt outcome = openAsync(connection);
                peer.awaitOpen();
                peer.sendClose();
                Throwable failure = outcome.await();
                assertTrue(failure instanceof java.net.ConnectException);
            }
        }
    }

    @Test(timeout = 15_000)
    public void connectionFailureBeforeOkayPreservesConnectionFailure() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OpenAttempt outcome = openAsync(connection);
                peer.awaitOpen();
                peer.dropConnection();
                Throwable failure = outcome.await();
                assertTrue(failure instanceof IOException);
                assertFalse(failure instanceof java.net.ConnectException);
            }
        }
    }

    @Test(timeout = 15_000)
    public void interruptedOpenClosesLateAcknowledgement() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                AtomicReference<Throwable> outcome = new AtomicReference<>();
                Thread opener = new Thread(() -> {
                    try {
                        connection.open("shell:wait");
                    } catch (Throwable throwable) {
                        outcome.set(throwable);
                    }
                });
                opener.start();
                peer.awaitOpen();
                opener.interrupt();
                opener.join(1_000);
                assertFalse(opener.isAlive());
                assertTrue(outcome.get() instanceof InterruptedException);
                peer.sendOkay();
                AdbProtocol.Message close = peer.readClientPacket();
                assertEquals(AdbProtocol.A_CLSE, close.command);
                assertEquals(peer.open.arg0, close.arg0);
                assertEquals(1, close.arg1);
                peer.closeRequested.countDown();
            }
        }
    }

    @Test(timeout = 15_000)
    public void okayAndCloseBeforeOpenReturnsLeavesEofStream() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(true, true)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OutputStream original = (OutputStream) field(connection, "mPlainOutputStream");
                CountDownLatch transmitted = new CountDownLatch(1);
                CountDownLatch release = new CountDownLatch(1);
                AtomicReference<AdbStream> registered = new AtomicReference<>();
                setField(connection, "mPlainOutputStream", new FilterOutputStream(original) {
                    @Override public void write(byte[] bytes) throws IOException {
                        try {
                            registered.set(pendingStream(connection));
                        } catch (Exception exception) {
                            throw new AssertionError(exception);
                        }
                        out.write(bytes);
                        out.flush();
                        transmitted.countDown();
                        awaitLatch(release, "OPEN sender was not released");
                    }
                });
                OpenAttempt attempt = null;
                try {
                    attempt = openAsync(connection);
                    assertTrue(transmitted.await(2, TimeUnit.SECONDS));
                    AdbStream stream = registered.get();
                    awaitCondition(() -> stream.isOpenAcknowledged() && stream.isClosed(),
                            "dispatcher did not process both OKAY and CLSE before OPEN returned");
                    assertTrue("OPEN returned before its gate was released", attempt.thread.isAlive());
                    release.countDown();
                    assertEquals(null, attempt.await());
                    assertEquals(stream, attempt.stream.get());
                    assertEquals(-1, stream.openInputStream().read());
                } finally {
                    release.countDown();
                    finishAttempt(attempt);
                    setField(connection, "mPlainOutputStream", original);
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void spuriousOpenNotificationDoesNotCompleteUnacknowledgedOpen() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OpenAttempt attempt = openAsync(connection);
                try {
                    peer.awaitOpen();
                    AdbStream pending = pendingStream(connection);
                    awaitThreadState(attempt.thread, Thread.State.WAITING, pending);
                    synchronized (pending) {
                        pending.notifyAll();
                        awaitThreadState(attempt.thread, Thread.State.BLOCKED, pending);
                    }
                    awaitThreadState(attempt.thread, Thread.State.WAITING, pending);
                    assertFalse(pending.isOpenAcknowledged());
                    peer.sendOkay();
                    assertEquals(null, attempt.await());
                    peer.closeRequested.countDown();
                } finally {
                    finishAttempt(attempt);
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void abandonedStreamRejectsAcknowledgementHeldByDispatcher() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OpenAttempt attempt = openAsync(connection);
                try {
                    peer.awaitOpen();
                    AdbStream pending = pendingStream(connection);
                    Thread dispatcher = (Thread) field(connection, "mConnectionThread");
                    IOException abandoned = new IOException("abandoned");
                    synchronized (pending) {
                        peer.sendOkay();
                        awaitThreadState(dispatcher, Thread.State.BLOCKED, pending);
                        // The dispatcher has already looked up this exact stream, so the
                        // unknown-stream branch cannot supply the cleanup being asserted.
                        pending.notifyFailure(abandoned);
                        assertTrue(openedStreams(connection).remove(peer.open.arg0, pending));
                    }
                    AdbProtocol.Message close = peer.readClientPacket();
                    assertEquals(AdbProtocol.A_CLSE, close.command);
                    assertEquals(peer.open.arg0, close.arg0);
                    assertEquals(1, close.arg1);
                    peer.closeRequested.countDown();
                    assertEquals(abandoned, attempt.await());
                } finally {
                    finishAttempt(attempt);
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void failedOpenTransmissionDoesNotRegisterStream() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer(false)) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                OutputStream original = (OutputStream) field(connection, "mPlainOutputStream");
                IOException transmissionFailure = new IOException("controlled OPEN write failure");
                AtomicReference<AdbStream> registered = new AtomicReference<>();
                setField(connection, "mPlainOutputStream", new OutputStream() {
                    @Override public void write(int value) throws IOException {
                        throw new AssertionError("expected packet write");
                    }
                    @Override public void write(byte[] packet) throws IOException {
                        try {
                            registered.set(pendingStream(connection));
                        } catch (Exception exception) {
                            throw new AssertionError(exception);
                        }
                        throw transmissionFailure;
                    }
                });
                try {
                    connection.open("shell:wait");
                    fail("OPEN should preserve the exact transmission failure");
                } catch (IOException expected) {
                    assertTrue(expected == transmissionFailure);
                    assertEquals(0, openedStreams(connection).size());
                    assertTrue(registered.get().isClosed());
                    try {
                        registered.get().read(new byte[1], 0, 1);
                        fail("abandoned stream must retain its failure");
                    } catch (IOException terminal) {
                        assertTrue(terminal == transmissionFailure);
                    }
                } finally {
                    setField(connection, "mPlainOutputStream", original);
                    // No OPEN was transmitted: terminate the intentionally incomplete
                    // exchange before connection teardown can race the peer's parser.
                    peer.dropConnection();
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void blockedFlushDoesNotBlockTerminalNotification() throws Exception {
        try (TestAdbPeer peer = new TestAdbPeer()) {
            peer.start();
            try (AdbConnection connection = connectionTo(peer)) {
                connection.connect();
                AdbStream stream = connection.open("shell:wait");
                Object outputLock = field(connection, "mLock");
                CountDownLatch flushStarted = new CountDownLatch(1);
                CountDownLatch releaseOutput = new CountDownLatch(1);
                Thread outputHolder = new Thread(() -> {
                    synchronized (outputLock) {
                        flushStarted.countDown();
                        try {
                            releaseOutput.await();
                        } catch (InterruptedException exception) {
                            Thread.currentThread().interrupt();
                        }
                    }
                });
                AtomicReference<Throwable> flushFailure = new AtomicReference<>();
                Thread flushing = new Thread(() -> {
                    try {
                        stream.flush();
                    } catch (Throwable failure) {
                        flushFailure.set(failure);
                    }
                });
                Thread terminal = new Thread(() -> stream.notifyClose(true));
                try {
                    outputHolder.start();
                    assertTrue(flushStarted.await(2, TimeUnit.SECONDS));
                    flushing.start();
                    awaitThreadState(flushing, Thread.State.BLOCKED, outputLock);
                    terminal.start();
                    terminal.join(2_000);
                    assertFalse("flush held the stream state monitor", terminal.isAlive());
                    assertTrue("output gate was released prematurely", flushing.isAlive());
                    releaseOutput.countDown();
                    flushing.join(2_000);
                    assertFalse(flushing.isAlive());
                    assertEquals(null, flushFailure.get());
                    peer.closeRequested.countDown();
                } finally {
                    releaseOutput.countDown();
                    finishThread(terminal);
                    finishThread(flushing);
                    finishThread(outputHolder);
                }
            }
        }
    }

    private static AdbStream pendingStream(AdbConnection connection) throws Exception {
        assertEquals(1, openedStreams(connection).size());
        return openedStreams(connection).values().iterator().next();
    }

    @SuppressWarnings("unchecked")
    private static ConcurrentHashMap<Integer, AdbStream> openedStreams(AdbConnection connection) throws Exception {
        return (ConcurrentHashMap<Integer, AdbStream>) field(connection, "mOpenedStreams");
    }

    private static Object field(Object object, String name) throws Exception {
        java.lang.reflect.Field field = object.getClass().getDeclaredField(name);
        field.setAccessible(true);
        return field.get(object);
    }

    private static void setField(Object object, String name, Object value) throws Exception {
        java.lang.reflect.Field field = object.getClass().getDeclaredField(name);
        field.setAccessible(true);
        field.set(object, value);
    }

    private static void awaitLatch(CountDownLatch latch, String message) throws IOException {
        try {
            if (!latch.await(2, TimeUnit.SECONDS)) throw new IOException(message);
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new IOException(exception);
        }
    }

    private static void awaitCondition(BooleanSupplier condition, String message) {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
        while (!condition.getAsBoolean()) {
            if (System.nanoTime() >= deadline) fail(message);
            Thread.yield();
        }
    }

    private static void awaitThreadState(Thread thread, Thread.State state, Object monitor) throws Exception {
        // These tests run on the host JVM, but Android's compilation bootclasspath
        // does not expose java.lang.management. Inspect the host monitor via reflection.
        Class<?> management = Class.forName("java.lang.management.ManagementFactory");
        Object bean = management.getMethod("getThreadMXBean").invoke(null);
        java.lang.reflect.Method getInfo = Class.forName("java.lang.management.ThreadMXBean")
                .getMethod("getThreadInfo", long.class);
        Class<?> infoType = Class.forName("java.lang.management.ThreadInfo");
        java.lang.reflect.Method getState = infoType.getMethod("getThreadState");
        java.lang.reflect.Method getLock = infoType.getMethod("getLockInfo");
        java.lang.reflect.Method getHash = Class.forName("java.lang.management.LockInfo")
                .getMethod("getIdentityHashCode");
        awaitCondition(() -> {
            try {
                Object info = getInfo.invoke(bean, thread.getId());
                if (info == null || getState.invoke(info) != state) return false;
                Object lock = getLock.invoke(info);
                return lock != null && (Integer) getHash.invoke(lock) == System.identityHashCode(monitor);
            } catch (ReflectiveOperationException exception) {
                throw new AssertionError("could not inspect host thread monitor", exception);
            }
        }, "thread " + thread.getName() + " never reached " + state + " on expected monitor");
    }

    private static void finishAttempt(OpenAttempt attempt) throws InterruptedException {
        if (attempt != null) finishThread(attempt.thread);
    }

    private static void finishThread(Thread thread) throws InterruptedException {
        if (thread.isAlive()) thread.interrupt();
        thread.join(2_000);
        assertFalse("test worker did not terminate: " + thread.getName(), thread.isAlive());
    }

    private static OpenAttempt openAsync(AdbConnection connection) {
        AtomicReference<Throwable> outcome = new AtomicReference<>();
        AtomicReference<AdbStream> stream = new AtomicReference<>();
        Thread opener = new Thread(() -> {
            try {
                stream.set(connection.open("shell:wait"));
            } catch (Throwable throwable) {
                outcome.set(throwable);
            }
        });
        opener.start();
        return new OpenAttempt(opener, outcome, stream);
    }

    private static final class OpenAttempt {
        final Thread thread;
        final AtomicReference<Throwable> outcome;
        final AtomicReference<AdbStream> stream;

        OpenAttempt(Thread thread, AtomicReference<Throwable> outcome, AtomicReference<AdbStream> stream) {
            this.thread = thread;
            this.outcome = outcome;
            this.stream = stream;
        }

        Throwable await() throws Exception {
            thread.join(1_000);
            assertFalse("open did not finish", thread.isAlive());
            return outcome.get();
        }
    }

    private static AdbConnection connectionTo(TestAdbPeer peer) throws IOException {
        java.security.KeyPair keyPair = generatedKeyPair();
        return AdbConnection.create("127.0.0.1", peer.getPort(), keyPair.getPrivate(),
                new TestCertificate(keyPair.getPublic()), 1);
    }

    private static void assertBoundsFailure(ThrowingRunnable runnable) throws Exception {
        try {
            runnable.run();
            fail("expected bounds failure");
        } catch (IndexOutOfBoundsException expected) {
        }
    }

    private interface ThrowingRunnable {
        void run() throws Exception;
    }

    private static java.security.KeyPair generatedKeyPair() {
        try {
            return KeyPairGenerator.getInstance("RSA").generateKeyPair();
        } catch (Exception exception) {
            throw new AssertionError(exception);
        }
    }

    private static final class TestAdbPeer implements AutoCloseable {
        private final ServerSocket serverSocket;
        private final CountDownLatch openAcknowledged = new CountDownLatch(1);
        private final CountDownLatch closeRequested = new CountDownLatch(1);
        private final CountDownLatch streamOpened = new CountDownLatch(1);
        private final AtomicReference<Throwable> workerFailure = new AtomicReference<>();
        private volatile boolean expectedDisconnect;
        private Socket socket;
        private AdbProtocol.Message open;
        private Thread thread;

        private final boolean acknowledgeImmediately;
        private final boolean closeImmediately;

        TestAdbPeer() throws IOException {
            this(true, false);
        }

        TestAdbPeer(boolean acknowledgeImmediately) throws IOException {
            this(acknowledgeImmediately, false);
        }

        TestAdbPeer(boolean acknowledgeImmediately, boolean closeImmediately) throws IOException {
            serverSocket = new ServerSocket(0);
            this.acknowledgeImmediately = acknowledgeImmediately;
            this.closeImmediately = closeImmediately;
        }

        int getPort() {
            return serverSocket.getLocalPort();
        }

        void start() {
            thread = new Thread(() -> {
                try (Socket acceptedSocket = serverSocket.accept()) {
                    socket = acceptedSocket;
                    socket.setSoTimeout(5_000);
                    AdbProtocol.Message.parse(socket.getInputStream(), AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
                    socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CNXN,
                            AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD, new byte[0]));
                    socket.getOutputStream().flush();
                    open = AdbProtocol.Message.parse(socket.getInputStream(),
                            AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
                    streamOpened.countDown();
                    if (acknowledgeImmediately) {
                        sendOkay();
                        if (closeImmediately) sendClose();
                    }
                    try {
                        if (!closeRequested.await(5, TimeUnit.SECONDS)) {
                            throw new AssertionError("test did not request peer completion");
                        }
                    } catch (InterruptedException exception) {
                        Thread.currentThread().interrupt();
                        throw new AssertionError(exception);
                    }
                    synchronized (this) {
                        socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CLSE, 1, open.arg0, null));
                        socket.getOutputStream().flush();
                    }
                } catch (Throwable throwable) {
                    if (!serverSocket.isClosed() && !expectedDisconnect) workerFailure.set(throwable);
                }
            }, "test-adb-peer");
            thread.start();
        }

        void closeStream() throws InterruptedException {
            if (!openAcknowledged.await(2, TimeUnit.SECONDS)) {
                throw new AssertionError("stream was not acknowledged");
            }
            closeRequested.countDown();
        }

        void awaitOpen() throws Exception {
            if (!streamOpened.await(2, TimeUnit.SECONDS)) throw new AssertionError("peer did not receive OPEN");
        }

        synchronized void sendOkay() throws IOException {
            socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_OKAY, 1, open.arg0, null));
            socket.getOutputStream().flush();
            openAcknowledged.countDown();
        }

        synchronized void sendClose() throws IOException {
            socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CLSE, 1, open.arg0, null));
            socket.getOutputStream().flush();
            closeRequested.countDown();
        }

        synchronized AdbProtocol.Message readClientPacket() throws IOException {
            return AdbProtocol.Message.parse(socket.getInputStream(), AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
        }

        void dropConnection() throws IOException {
            expectedDisconnect = true;
            socket.close();
            closeRequested.countDown();
        }

        void sendPayloadAndClose(byte[] payload) throws Exception {
            if (!streamOpened.await(2, TimeUnit.SECONDS)) {
                throw new AssertionError("stream was not opened");
            }
            synchronized (this) {
                socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_WRTE, 1, open.arg0, payload));
                socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CLSE, 1, open.arg0, null));
                socket.getOutputStream().flush();
                closeRequested.countDown();
            }
        }

        @Override
        public void close() throws Exception {
            serverSocket.close();
            if (socket != null) socket.close();
            if (thread != null) {
                thread.join(2_000);
                assertFalse("peer thread did not terminate", thread.isAlive());
            }
            Throwable failure = workerFailure.get();
            if (failure != null) throw new AssertionError("peer failed", failure);
        }
    }

    private static final class TestCertificate extends Certificate {
        private final PublicKey publicKey;

        TestCertificate(PublicKey publicKey) {
            super("X.509");
            this.publicKey = publicKey;
        }

        @Override public byte[] getEncoded() { return new byte[0]; }
        @Override public void verify(PublicKey key) { }
        @Override public void verify(PublicKey key, String provider) { }
        @Override public String toString() { return "test"; }
        @Override public PublicKey getPublicKey() { return publicKey; }
    }
}
