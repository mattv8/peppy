package io.github.muntashirakon.adb;

import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.Test;
import org.junit.runner.RunWith;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.net.Socket;
import java.net.SocketAddress;
import java.io.IOException;
import java.security.KeyPairGenerator;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.cert.Certificate;

@RunWith(AndroidJUnit4.class)
public class PairingAuthCtxTest {
    @Test
    public void rejectsHostnamesWithoutDns() {
        assertTrue(PairingConnectionCtx.parseNumericHost("127.0.0.1").isLoopbackAddress());
        assertThrows(IllegalArgumentException.class,
                () -> PairingConnectionCtx.parseNumericHost("localhost"));
    }

    @Test
    public void destroyIsIdempotentAndSerializedWithUse() throws Exception {
        PairingAuthCtx context = PairingAuthCtx.createAlice("000000".getBytes("UTF-8"));
        assertNotNull(context);
        CountDownLatch started = new CountDownLatch(1);
        Thread worker = new Thread(() -> {
            started.countDown();
            for (int index = 0; index < 100; index++) context.encrypt(new byte[16]);
        });
        worker.start();
        assertTrue(started.await(1, TimeUnit.SECONDS));
        context.destroy();
        context.destroy();
        worker.join(1_000);
        assertTrue(context.isDestroyed());
    }

    @Test
    public void processFailureCanBeDestroyedTwice() throws Exception {
        PairingAuthCtx context = PairingAuthCtx.createAlice("000000".getBytes("UTF-8"));
        assertNotNull(context);

        assertThrows(IllegalStateException.class, () -> context.initCipher(new byte[1]));
        context.destroy();
        context.destroy();
        assertTrue(context.isDestroyed());
    }

    @Test
    public void processMessageAndDestroyDoNotRaceNativeContext() throws Exception {
        PairingAuthCtx context = PairingAuthCtx.createAlice("000000".getBytes("UTF-8"));
        assertNotNull(context);
        CountDownLatch started = new CountDownLatch(1);
        Thread worker = new Thread(() -> {
            started.countDown();
            try {
                context.initCipher(new byte[1]);
            } catch (IllegalStateException expected) {
            }
        });
        worker.start();
        assertTrue(started.await(1, TimeUnit.SECONDS));
        context.destroy();
        worker.join(1_000);
        context.destroy();
        assertTrue(context.isDestroyed());
    }

    @Test
    public void startConnectFailureClosesFactorySocket() throws Exception {
        java.security.KeyPair generated = KeyPairGenerator.getInstance("RSA").generateKeyPair();
        BlockingSocket socket = new BlockingSocket();
        PairingConnectionCtx context = new PairingConnectionCtx("127.0.0.1", 1, new byte[] {1},
                new KeyPair(generated.getPrivate(), new TestCertificate(generated.getPublic())), "test", 10_000,
                () -> socket);
        assertThrows(IOException.class, context::start);
        assertTrue(socket.closed.get());
    }

    private static final class BlockingSocket extends Socket {
        final AtomicBoolean closed = new AtomicBoolean();

        @Override
        public void connect(SocketAddress endpoint, int timeout) throws IOException {
            throw new IOException("connect failed");
        }

        @Override
        public synchronized void close() {
            closed.set(true);
        }
    }

    private static final class TestCertificate extends Certificate {
        private final PublicKey publicKey;

        TestCertificate(PublicKey publicKey) { super("X.509"); this.publicKey = publicKey; }
        @Override public byte[] getEncoded() { return new byte[0]; }
        @Override public void verify(PublicKey key) { }
        @Override public void verify(PublicKey key, String provider) { }
        @Override public String toString() { return "test"; }
        @Override public PublicKey getPublicKey() { return publicKey; }
    }
}
