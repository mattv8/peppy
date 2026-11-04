// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import java.io.IOException;
import java.net.Socket;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;

/** Owns exactly one socket and makes close/publication races safe. */
final class PairingSocketOwner {
    private final AtomicReference<Socket> mSocket = new AtomicReference<>();
    private final Runnable mAfterPublish;

    PairingSocketOwner() {
        this(() -> { });
    }

    PairingSocketOwner(Runnable afterPublish) {
        mAfterPublish = afterPublish;
    }

    void publish(Socket socket, AtomicBoolean closed) throws IOException {
        if (!mSocket.compareAndSet(null, socket)) {
            socket.close();
            throw new IOException("Connection already owns a socket.");
        }
        mAfterPublish.run();
        if (closed.get()) {
            if (mSocket.compareAndSet(socket, null)) socket.close();
            throw new IOException("Connection closed.");
        }
    }

    void handoff(Socket socket, Socket replacement, AtomicBoolean closed) throws IOException {
        if (!mSocket.compareAndSet(socket, replacement)) {
            replacement.close();
            throw new IOException("Connection closed.");
        }
        mAfterPublish.run();
        if (closed.get()) {
            if (mSocket.compareAndSet(replacement, null)) replacement.close();
            throw new IOException("Connection closed.");
        }
    }

    void close() {
        Socket socket = mSocket.getAndSet(null);
        if (socket == null) return;
        try {
            socket.close();
        } catch (IOException ignore) {
        }
    }
}
