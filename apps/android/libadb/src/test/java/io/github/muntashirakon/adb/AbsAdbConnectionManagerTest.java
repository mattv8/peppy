package io.github.muntashirakon.adb;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.IOException;
import java.security.KeyPairGenerator;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

public class AbsAdbConnectionManagerTest {
    @Test
    public void cancelPairingWhilePendingRejectsTheAttempt() throws Exception {
        TestManager manager = new TestManager();
        manager.pauseAfterPending = true;
        AtomicReference<Throwable> failure = new AtomicReference<>();
        Thread pairing = new Thread(() -> {
            try {
                manager.pair("127.0.0.1", 1234, "123456");
            } catch (Throwable throwable) {
                failure.set(throwable);
            }
        });
        pairing.start();
        assertTrue(manager.pending.await(1, TimeUnit.SECONDS));

        manager.cancelPairing();
        manager.resume.countDown();
        pairing.join(1_000);

        assertTrue(failure.get() instanceof IOException);
        assertEquals(0, manager.started.get());
    }

    @Test
    public void cancelPairingWhileActiveClosesTheAttempt() throws Exception {
        TestManager manager = new TestManager();
        AtomicReference<Throwable> failure = new AtomicReference<>();
        Thread pairing = new Thread(() -> {
            try {
                manager.pair("127.0.0.1", 1234, "123456");
            } catch (Throwable throwable) {
                failure.set(throwable);
            }
        });
        pairing.start();
        assertTrue(manager.startedLatch.await(1, TimeUnit.SECONDS));

        manager.cancelPairing();
        pairing.join(1_000);

        assertTrue(failure.get() instanceof IOException);
        assertTrue(manager.closed.get() >= 1);
    }

    @Test
    public void rejectedPairingCodeDoesNotPoisonLaterPairing() throws Exception {
        TestManager manager = new TestManager();

        assertThrows(NullPointerException.class, () -> manager.pair("127.0.0.1", 1234, null));
        manager.allowSuccess = true;

        assertTrue(manager.pair("127.0.0.1", 1234, "123456"));
    }

    @Test
    public void passwordBytesAreWipedWhenPairingIsAlreadyActive() throws Exception {
        TestManager manager = new TestManager();
        AtomicReference<Throwable> failure = new AtomicReference<>();
        Thread pairing = new Thread(() -> {
            try {
                manager.pair("127.0.0.1", 1234, "123456");
            } catch (Throwable throwable) {
                failure.set(throwable);
            }
        });
        pairing.start();
        assertTrue(manager.startedLatch.await(1, TimeUnit.SECONDS));

        assertThrows(IllegalStateException.class, () -> manager.pair("127.0.0.1", 1234, "654321"));
        assertArrayEquals(new byte[6], manager.lastPassword);
        manager.cancelPairing();
        pairing.join(1_000);
        assertTrue(failure.get() instanceof IOException);
    }

    @Test
    public void cancelPairingDoesNotWaitForManagerLockHeldByBlockedOpen() throws Exception {
        try (BlockedOpenPeer peer = new BlockedOpenPeer()) {
            peer.start();
            TestManager manager = new TestManager();
            assertTrue(manager.connect("127.0.0.1", peer.getPort()));
            AtomicReference<Throwable> openFailure = new AtomicReference<>();
            Thread opener = new Thread(() -> {
                try {
                    manager.openStream("shell:blocked");
                } catch (Throwable throwable) {
                    openFailure.set(throwable);
                }
            });
            opener.start();
            assertTrue(peer.openReceived.await(1, TimeUnit.SECONDS));

            Thread cancellation = new Thread(manager::cancelPairing);
            cancellation.start();
            cancellation.join(1_000);
            assertTrue("cancellation waited for blocked OPEN", !cancellation.isAlive());
            assertEquals("peer closed before manager teardown", 1, peer.clientEof.getCount());

            manager.close();
            assertTrue("manager close did not close the peer socket", peer.clientEof.await(1, TimeUnit.SECONDS));
            opener.join(1_000);
            assertTrue("OPEN did not terminate after manager close", !opener.isAlive());
            assertTrue(openFailure.get() instanceof IOException);
        }
    }

    private static final class TestManager extends AbsAdbConnectionManager {
        final CountDownLatch pending = new CountDownLatch(1);
        final CountDownLatch resume = new CountDownLatch(1);
        final CountDownLatch startedLatch = new CountDownLatch(1);
        final AtomicInteger started = new AtomicInteger();
        final AtomicInteger closed = new AtomicInteger();
        boolean pauseAfterPending;
        boolean allowSuccess;
        byte[] lastPassword;

        @Override
        void onPairingPendingRegistered() {
            pending.countDown();
            if (pauseAfterPending) {
                try {
                    resume.await();
                } catch (InterruptedException exception) {
                    Thread.currentThread().interrupt();
                    throw new AssertionError(exception);
                }
            }
        }

        @Override
        byte[] getPairingCodeBytes(String pairingCode) {
            lastPassword = pairingCode.getBytes(java.nio.charset.StandardCharsets.UTF_8);
            return lastPassword;
        }

        @Override
        PairingAttempt createPairingAttempt(String host, int port, byte[] password, KeyPair keyPair, String deviceName) {
            return new PairingAttempt() {
                @Override
                public void start() throws IOException {
                    started.incrementAndGet();
                    startedLatch.countDown();
                    if (allowSuccess) return;
                    try {
                        if (!resume.await(1, TimeUnit.SECONDS)) throw new IOException("pairing did not close");
                    } catch (InterruptedException exception) {
                        Thread.currentThread().interrupt();
                        throw new IOException(exception);
                    }
                    throw new IOException("pairing cancelled");
                }

                @Override
                public void close() {
                    closed.incrementAndGet();
                    resume.countDown();
                }
            };
        }

        @Override
        protected PrivateKey getPrivateKey() {
            return generatedKeyPair().getPrivate();
        }

        @Override
        protected Certificate getCertificate() {
            return new TestCertificate(generatedKeyPair().getPublic());
        }

        @Override
        protected String getDeviceName() {
            return "Peppy setup";
        }
    }

    private static java.security.KeyPair generatedKeyPair() {
        try {
            return KeyPairGenerator.getInstance("RSA").generateKeyPair();
        } catch (Exception exception) {
            throw new AssertionError(exception);
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

    private static final class BlockedOpenPeer implements AutoCloseable {
        final java.net.ServerSocket server = new java.net.ServerSocket(0);
        final CountDownLatch openReceived = new CountDownLatch(1);
        final CountDownLatch clientEof = new CountDownLatch(1);
        final AtomicReference<Throwable> failure = new AtomicReference<>();
        volatile java.net.Socket socket;
        volatile boolean cleanup;
        Thread thread;

        BlockedOpenPeer() throws IOException {
        }

        int getPort() {
            return server.getLocalPort();
        }

        void start() {
            thread = new Thread(() -> {
                try (java.net.Socket accepted = server.accept()) {
                    socket = accepted;
                    socket.setSoTimeout(5_000);
                    AdbProtocol.Message.parse(socket.getInputStream(), AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
                    socket.getOutputStream().write(AdbProtocol.generateMessage(AdbProtocol.A_CNXN,
                            AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD, new byte[0]));
                    socket.getOutputStream().flush();
                    AdbProtocol.Message open = AdbProtocol.Message.parse(socket.getInputStream(), AdbProtocol.A_VERSION_MIN, AdbProtocol.MAX_PAYLOAD);
                    if (open.command != AdbProtocol.A_OPEN) {
                        throw new AssertionError("expected OPEN packet");
                    }
                    openReceived.countDown();
                    if (socket.getInputStream().read() != -1) {
                        throw new AssertionError("expected client EOF after manager close");
                    }
                    clientEof.countDown();
                } catch (Throwable throwable) {
                    if (!cleanup) failure.set(throwable);
                }
            });
            thread.start();
        }

        @Override
        public void close() throws Exception {
            cleanup = true;
            server.close();
            if (socket != null) socket.close();
            if (thread != null) {
                thread.join(1_000);
                assertTrue("peer did not terminate", !thread.isAlive());
            }
            if (failure.get() != null) throw new AssertionError("peer failed", failure.get());
        }
    }
}
