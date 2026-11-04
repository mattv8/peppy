package io.github.muntashirakon.adb;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.IOException;
import java.net.Socket;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

public class PairingSocketOwnerTest {
    @Test
    public void publishThenCloseIsDetectedAndSocketIsClosed() {
        AtomicBoolean closed = new AtomicBoolean();
        TestSocket socket = new TestSocket();
        AtomicReference<PairingSocketOwner> ownerRef = new AtomicReference<>();
        PairingSocketOwner owner = new PairingSocketOwner(() -> {
            closed.set(true);
            ownerRef.get().close();
        });
        ownerRef.set(owner);

        assertThrows(IOException.class, () -> owner.publish(socket, closed));
        assertEquals(1, socket.closeCalls.get());
    }

    @Test
    public void closeBeforePublicationClosesPublishedSocket() {
        AtomicBoolean closed = new AtomicBoolean(true);
        TestSocket socket = new TestSocket();

        assertThrows(IOException.class, () -> new PairingSocketOwner().publish(socket, closed));
        assertEquals(1, socket.closeCalls.get());
    }

    @Test
    public void handoffThenCloseIsDetectedAndReplacementIsClosed() throws IOException {
        AtomicBoolean closed = new AtomicBoolean();
        TestSocket original = new TestSocket();
        TestSocket replacement = new TestSocket();
        AtomicInteger publications = new AtomicInteger();
        PairingSocketOwner owner = new PairingSocketOwner(() -> {
            if (publications.incrementAndGet() == 2) closed.set(true);
        });
        owner.publish(original, closed);

        assertThrows(IOException.class, () -> owner.handoff(original, replacement, closed));
        assertEquals(1, replacement.closeCalls.get());
    }

    @Test
    public void closeIsIdempotent() throws IOException {
        AtomicBoolean closed = new AtomicBoolean();
        TestSocket socket = new TestSocket();
        PairingSocketOwner owner = new PairingSocketOwner();
        owner.publish(socket, closed);

        owner.close();
        owner.close();

        assertEquals(1, socket.closeCalls.get());
        assertTrue(closed.compareAndSet(false, true));
    }

    @Test
    public void socketFactorySocketUnblocksWhenOwnedSocketIsClosed() throws Exception {
        AtomicBoolean closed = new AtomicBoolean();
        BlockingSocket socket = new BlockingSocket();
        PairingConnectionCtx.SocketFactory socketFactory = () -> socket;
        PairingSocketOwner owner = new PairingSocketOwner();
        owner.publish(socketFactory.create(), closed);
        Thread connect = new Thread(socket::blockUntilClosed);
        connect.start();

        owner.close();
        connect.join(1_000);

        assertTrue(socket.wasUnblocked.get());
    }

    private static class TestSocket extends Socket {
        final AtomicInteger closeCalls = new AtomicInteger();

        @Override
        public synchronized void close() {
            closeCalls.incrementAndGet();
        }
    }

    private static final class BlockingSocket extends TestSocket {
        final AtomicBoolean wasUnblocked = new AtomicBoolean();

        void blockUntilClosed() {
            while (closeCalls.get() == 0) Thread.yield();
            wasUnblocked.set(true);
        }
    }
}
