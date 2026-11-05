// SPDX-License-Identifier: BSD-3-Clause AND (GPL-3.0-or-later OR Apache-2.0)

package io.github.muntashirakon.adb;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.Timeout;

import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.security.KeyPairGenerator;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.FutureTask;
import java.util.concurrent.TimeUnit;

public class AdbStreamStateTest {
    @Rule
    public final Timeout timeout = Timeout.seconds(15);

    @Test
    public void queuedPayloadsAndPartialCurrentBufferDrainBeforeRepeatedEof() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream stream = fixture.stream();
            stream.addPayload(new byte[]{1, 2, 3});
            stream.addPayload(new byte[]{4, 5});
            stream.addPayload(new byte[]{6});
            byte[] bytes = new byte[4];
            assertEquals(1, stream.read(bytes, 0, 1));
            assertEquals(1, bytes[0]);
            stream.notifyClose(true);
            assertTrue(stream.isClosed());
            assertEquals(2, stream.available());
            assertEquals(2, stream.read(bytes, 1, 3));
            assertArrayEquals(new byte[]{1, 2, 3, 0}, bytes);
            assertEquals(2, stream.read(bytes, 0, 4));
            assertEquals(4, bytes[0]);
            assertEquals(5, bytes[1]);
            assertEquals(1, stream.read(bytes, 0, 4));
            assertEquals(6, bytes[0]);
            assertEquals(0, stream.available());
            assertEquals(-1, stream.read(bytes, 0, 4));
            assertEquals(-1, stream.read(bytes, 0, 4));
        }
    }

    @Test
    public void wrapperPreservesBufferedBytesAndReadsHighBytesUnsignedAfterPeerClose() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream stream = fixture.stream();
            AdbInputStream input = stream.openInputStream();
            stream.addPayload(new byte[]{7, (byte) 0x80, (byte) 0xff});
            stream.addPayload(new byte[]{8});
            assertEquals(7, input.read());
            stream.notifyClose(true);
            assertEquals(2, input.available());
            assertEquals(128, input.read());
            assertEquals(255, input.read());
            assertEquals(8, input.read());
            assertEquals(-1, input.read());
            assertEquals(-1, input.read());
        }
    }

    @Test
    public void localCloseDrainsReceivedBytesThenKeepsFailureDespitePeerClose() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream stream = fixture.stream();
            AdbInputStream input = stream.openInputStream();
            stream.addPayload(new byte[]{1, 2});
            stream.addPayload(new byte[]{3});
            assertEquals(1, input.read());
            stream.close();
            assertTrue(stream.isClosed());
            assertEquals(2, input.read());
            assertEquals(3, input.read());
            IOException failure = expectIOException(input::read);
            stream.notifyClose(true);
            stream.close();
            assertSame(failure, expectIOException(input::read));
            assertSame(failure, expectIOException(stream::flush));
        }
    }

    @Test
    public void connectionFailureDrainsBytesThenPreservesExactCauseThroughCleanup() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream stream = fixture.stream();
            AdbInputStream input = stream.openInputStream();
            stream.addPayload(new byte[]{1, 2});
            stream.addPayload(new byte[]{3});
            assertEquals(1, input.read());
            IOException failure = new IOException("original transport failure");
            stream.notifyFailure(failure);
            stream.notifyClose(true);
            stream.notifyClose(false);
            stream.notifyFailure(new IOException("later cleanup"));
            stream.close();
            assertTrue(stream.isClosed());
            assertEquals(2, input.read());
            assertEquals(3, input.read());
            assertSame(failure, expectIOException(input::read));
            assertSame(failure, expectIOException(input::read));
            assertSame(failure, expectIOException(() -> stream.write(new byte[]{9}, 0, 1)));
        }
    }

    @Test
    public void peerEofWinsOverLaterFailureAndLocalCleanup() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream stream = fixture.stream();
            stream.notifyClose(true);
            assertEquals(-1, stream.openInputStream().read());
            stream.notifyFailure(new IOException("late failure"));
            stream.notifyClose(false);
            stream.close();
            assertTrue(stream.isClosed());
            assertEquals(-1, stream.openInputStream().read());
            assertEquals(-1, stream.read(new byte[1], 0, 1));
        }
    }

    @Test
    public void blockedReadersWakeForPeerCloseLocalCloseAndFailure() throws Exception {
        try (Fixture fixture = new Fixture()) {
            for (Terminal terminal : Terminal.values()) {
                AdbStream stream = fixture.stream();
                IOException failure = new IOException("reader transport failure");
                try (BlockedCall reader = new BlockedCall(() -> stream.openInputStream().read())) {
                    reader.awaitWaiting();
                    terminal.apply(stream, failure);
                    Outcome outcome = reader.result();
                    if (terminal == Terminal.PEER) {
                        assertEquals(-1, outcome.value);
                        assertEquals(null, outcome.failure);
                    } else {
                        assertTrue(outcome.failure instanceof IOException);
                        if (terminal == Terminal.FAILURE) assertSame(failure, outcome.failure);
                    }
                }
            }
        }
    }

    @Test
    public void writersBlockedOnConsumedCreditWakeAndFailForEveryTerminalState() throws Exception {
        try (Fixture fixture = new Fixture()) {
            for (Terminal terminal : Terminal.values()) {
                AdbStream stream = fixture.stream();
                fixture.consumeWriteCredit(stream);
                IOException failure = new IOException("writer transport failure");
                try (BlockedCall writer = new BlockedCall(() -> {
                    stream.write(new byte[]{2}, 0, 1);
                    return 0;
                })) {
                    writer.awaitWaiting();
                    terminal.apply(stream, failure);
                    Outcome outcome = writer.result();
                    assertTrue(outcome.failure instanceof IOException);
                    if (terminal == Terminal.FAILURE) assertSame(failure, outcome.failure);
                    assertTrue(stream.isClosed());
                    expectIOException(() -> stream.write(new byte[]{3}, 0, 1));
                }
            }
        }
    }

    @Test
    public void interruptedReaderAndWriterRestoreInterruptAndKeepDurableFailure() throws Exception {
        try (Fixture fixture = new Fixture()) {
            for (boolean writing : new boolean[]{false, true}) {
                AdbStream stream = fixture.stream();
                if (writing) fixture.consumeWriteCredit(stream);
                try (BlockedCall call = new BlockedCall(() -> {
                    if (!writing) return stream.openInputStream().read();
                    stream.write(new byte[]{2}, 0, 1);
                    return 0;
                })) {
                    call.awaitWaiting();
                    call.thread.interrupt();
                    Outcome outcome = call.result();
                    assertTrue(outcome.failure instanceof IOException);
                    assertTrue(outcome.failure.getCause() instanceof InterruptedException);
                    assertTrue(outcome.interrupted);
                    assertTrue(stream.isClosed());
                    stream.notifyClose(true);
                    stream.notifyClose(false);
                    stream.close();
                    assertSame(outcome.failure, expectIOException(() -> stream.read(new byte[1], 0, 1)));
                    assertSame(outcome.failure, expectIOException(() -> stream.write(new byte[]{3}, 0, 1)));
                }
            }
        }
    }

    @Test(timeout = 15_000)
    public void competingTerminalOutcomeWinsOverInterruptedWaiter() throws Exception {
        try (Fixture fixture = new Fixture()) {
            AdbStream reader = fixture.stream();
            IOException readFailure = new IOException("chosen reader failure");
            try (BlockedCall call = new BlockedCall(() -> reader.read(new byte[1], 0, 1))) {
                call.awaitWaiting();
                synchronized (reader) {
                    call.thread.interrupt();
                    reader.notifyFailure(readFailure);
                }
                Outcome outcome = call.result();
                assertSame(readFailure, outcome.failure);
                assertTrue(outcome.interrupted);
            }

            AdbStream writer = fixture.stream();
            fixture.consumeWriteCredit(writer);
            try (BlockedCall call = new BlockedCall(() -> {
                writer.write(new byte[]{1}, 0, 1);
                return 0;
            })) {
                call.awaitWaiting();
                synchronized (writer) {
                    call.thread.interrupt();
                    writer.notifyClose(true);
                }
                Outcome outcome = call.result();
                assertTrue(outcome.failure instanceof IOException);
                assertFalse(outcome.failure.getCause() instanceof InterruptedException);
                assertTrue(outcome.interrupted);
            }
        }
    }

    @Test
    public void readsValidateNullAndOverflowBoundsAndAllowZeroLengthInEveryState() throws Exception {
        try (Fixture fixture = new Fixture()) {
            for (Terminal terminal : Terminal.values()) {
                AdbStream stream = fixture.stream();
                assertReadArguments(stream);
                terminal.apply(stream, new IOException("terminal failure"));
                assertReadArguments(stream);
            }
        }
    }

    private static void assertReadArguments(AdbStream stream) throws Exception {
        AdbInputStream input = stream.openInputStream();
        byte[] bytes = new byte[2];
        expectException(NullPointerException.class, () -> stream.read(null, 0, 0));
        expectException(NullPointerException.class, () -> input.read(null, 0, 0));
        expectException(NullPointerException.class, () -> input.read(null));
        int[][] invalidBounds = {{-1, 1}, {0, -1}, {1, 2}, {3, 0},
                {Integer.MAX_VALUE, 1}, {1, Integer.MAX_VALUE}, {Integer.MAX_VALUE, Integer.MAX_VALUE}};
        for (int[] bounds : invalidBounds) {
            expectException(IndexOutOfBoundsException.class, () -> stream.read(bytes, bounds[0], bounds[1]));
            expectException(IndexOutOfBoundsException.class, () -> input.read(bytes, bounds[0], bounds[1]));
        }
        assertEquals(0, stream.read(bytes, 0, 0));
        assertEquals(0, stream.read(bytes, 2, 0));
        assertEquals(0, input.read(bytes, 2, 0));
        assertEquals(0, input.read(new byte[0]));
    }

    private static IOException expectIOException(ThrowingCall call) throws Exception {
        return expectException(IOException.class, call);
    }

    private static <T extends Throwable> T expectException(Class<T> type, ThrowingCall call) throws Exception {
        try {
            call.run();
        } catch (Throwable failure) {
            if (type.isInstance(failure)) return type.cast(failure);
            throw new AssertionError("Expected " + type.getSimpleName(), failure);
        }
        throw new AssertionError("Expected " + type.getSimpleName());
    }

    private interface ThrowingCall {
        void run() throws Exception;
    }

    private interface ReadOrWrite {
        int run() throws Exception;
    }

    private enum Terminal {
        PEER, LOCAL, FAILURE;

        void apply(AdbStream stream, IOException failure) throws IOException {
            if (this == PEER) stream.notifyClose(true);
            else if (this == LOCAL) stream.close();
            else stream.notifyFailure(failure);
        }
    }

    private static final class Outcome {
        int value;
        Throwable failure;
        boolean interrupted;
    }

    private static final class BlockedCall implements AutoCloseable {
        private final CountDownLatch started = new CountDownLatch(1);
        private final FutureTask<Outcome> task;
        final Thread thread;

        BlockedCall(ReadOrWrite operation) {
            task = new FutureTask<>(() -> {
                Outcome outcome = new Outcome();
                started.countDown();
                try {
                    outcome.value = operation.run();
                } catch (Throwable failure) {
                    outcome.failure = failure;
                } finally {
                    outcome.interrupted = Thread.currentThread().isInterrupted();
                }
                return outcome;
            });
            thread = new Thread(task, "adb-state-blocked-call");
            thread.start();
        }

        void awaitWaiting() throws Exception {
            assertTrue("worker did not start", started.await(2, TimeUnit.SECONDS));
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
            while (System.nanoTime() < deadline) {
                if (thread.getState() == Thread.State.WAITING) return;
                if (task.isDone()) fail("operation returned before terminal notification");
                Thread.yield();
            }
            fail("operation did not block awaiting stream state");
        }

        Outcome result() throws Exception {
            return task.get(2, TimeUnit.SECONDS);
        }

        @Override
        public void close() throws Exception {
            thread.interrupt();
            thread.join(2_000);
            assertFalse("blocked worker leaked", thread.isAlive());
        }
    }

    private static final class Fixture implements AutoCloseable {
        private final ServerSocket server = new ServerSocket(0, 1, InetAddress.getLoopbackAddress());
        private Socket peer;
        private AdbConnection connection;
        private int localId;

        Fixture() throws Exception {
            try {
                server.setSoTimeout(2_000);
                java.security.KeyPair keys = KeyPairGenerator.getInstance("RSA").generateKeyPair();
                connection = AdbConnection.create(server.getInetAddress().getHostAddress(), server.getLocalPort(),
                        keys.getPrivate(), new TestCertificate(keys.getPublic()), 1);
                peer = server.accept();
                peer.setSoTimeout(2_000);
                // The peer advertises its connection before connect waits; no helper thread is needed.
                peer.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CNXN,
                        AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD, new byte[0]));
                peer.getOutputStream().flush();
                assertTrue(connection.connect(2, TimeUnit.SECONDS, false));
                assertEquals(AdbProtocol.A_CNXN, packet().command);
            } catch (Throwable failure) {
                try {
                    close();
                } catch (Throwable cleanupFailure) {
                    failure.addSuppressed(cleanupFailure);
                }
                throw failure;
            }
        }

        AdbStream stream() throws Exception {
            AdbStream stream = new AdbStream(connection, ++localId);
            stream.acknowledgeOpen(100 + localId);
            return stream;
        }

        void consumeWriteCredit(AdbStream stream) throws Exception {
            stream.write(new byte[]{1}, 0, 1);
            stream.flush();
            AdbProtocol.Message message;
            do {
                message = packet();
            } while (message.command == AdbProtocol.A_CLSE);
            assertEquals(AdbProtocol.A_WRTE, message.command);
            assertArrayEquals(new byte[]{1}, message.payload);
        }

        private AdbProtocol.Message packet() throws Exception {
            return AdbProtocol.Message.parse(peer.getInputStream(),
                    AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
        }

        @Override
        public void close() throws Exception {
            try {
                if (connection != null) connection.close();
            } finally {
                try {
                    if (peer != null) peer.close();
                } finally {
                    server.close();
                }
            }
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
