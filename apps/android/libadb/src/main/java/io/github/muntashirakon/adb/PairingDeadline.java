// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import java.io.Closeable;
import java.io.IOException;

/** One total pairing deadline which closes the currently owned socket when it expires. */
final class PairingDeadline implements Closeable {
    private final long mEndsAtNanos;
    private final Thread mWatchdog;

    PairingDeadline(int timeoutMillis, Runnable onExpiry) {
        mEndsAtNanos = System.nanoTime() + timeoutMillis * 1_000_000L;
        mWatchdog = new Thread(() -> {
            try {
                Thread.sleep(timeoutMillis);
                onExpiry.run();
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
        }, "adb-pairing-deadline");
        mWatchdog.setDaemon(true);
    }

    void start() {
        mWatchdog.start();
    }

    int remainingMillis() throws IOException {
        long remainingNanos = mEndsAtNanos - System.nanoTime();
        if (remainingNanos <= 0) throw new IOException("Pairing deadline exceeded.");
        return (int) Math.max(1L, (remainingNanos + 999_999L) / 1_000_000L);
    }

    void check() throws IOException {
        remainingMillis();
    }

    @Override
    public void close() {
        mWatchdog.interrupt();
    }
}
