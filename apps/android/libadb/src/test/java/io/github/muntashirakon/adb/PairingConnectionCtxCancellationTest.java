package io.github.muntashirakon.adb;

import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.IOException;
import java.net.InetAddress;
import java.net.Socket;
import java.net.SocketAddress;
import java.security.KeyPairGenerator;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;

public class PairingConnectionCtxCancellationTest {
    @Test
    public void closeCancelsAConnectInProgress() throws Exception {
        java.security.KeyPair generated = KeyPairGenerator.getInstance("RSA").generateKeyPair();
        BlockingConnectSocket socket = new BlockingConnectSocket();
        PairingConnectionCtx context = new PairingConnectionCtx(InetAddress.getLoopbackAddress(), 1234, new byte[] {1},
                new KeyPair(generated.getPrivate(), new TestCertificate(generated.getPublic())), "Peppy setup", 10_000,
                () -> socket, null);
        AtomicReference<Throwable> failure = new AtomicReference<>();
        Thread pairing = new Thread(() -> {
            try {
                context.start();
            } catch (Throwable throwable) {
                failure.set(throwable);
            }
        });
        pairing.start();
        assertTrue(socket.connecting.await(1, TimeUnit.SECONDS));

        context.close();
        pairing.join(1_000);

        assertTrue(socket.closed.get());
        assertTrue(failure.get() instanceof IOException);
        assertFalse(failure.get() instanceof AdbPairingCodeRejectedException);
    }

    @Test
    public void timeoutDuringConnectIsNotAPairingCodeRejection() throws Exception {
        java.security.KeyPair generated = KeyPairGenerator.getInstance("RSA").generateKeyPair();
        BlockingConnectSocket socket = new BlockingConnectSocket();
        PairingConnectionCtx context = new PairingConnectionCtx(InetAddress.getLoopbackAddress(), 1234, new byte[] {1},
                new KeyPair(generated.getPrivate(), new TestCertificate(generated.getPublic())), "Peppy setup", 20,
                () -> socket, null);

        IOException failure = assertThrows(IOException.class, context::start);

        assertTrue(socket.closed.get());
        assertFalse(failure instanceof AdbPairingCodeRejectedException);
    }

    private static final class BlockingConnectSocket extends Socket {
        final CountDownLatch connecting = new CountDownLatch(1);
        final CountDownLatch closedLatch = new CountDownLatch(1);
        final AtomicBoolean closed = new AtomicBoolean();

        @Override
        public void connect(SocketAddress endpoint, int timeout) throws IOException {
            connecting.countDown();
            try {
                closedLatch.await();
            } catch (InterruptedException exception) {
                Thread.currentThread().interrupt();
                throw new IOException(exception);
            }
            throw new IOException("socket closed");
        }

        @Override
        public synchronized void close() {
            closed.set(true);
            closedLatch.countDown();
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
