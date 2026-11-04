// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import java.io.IOException;

/** State protected by {@link AbsAdbConnectionManager}'s lock for cancellable pairing attempts. */
final class PairingAttemptRegistry<T> {
    private T mActive;
    private Object mPending;
    private boolean mCancellationRequested;

    Object begin() {
        if (mPending != null) throw new IllegalStateException("Pairing is already active.");
        mPending = new Object();
        return mPending;
    }

    void activate(Object pending, T attempt) throws IOException {
        if (mPending != pending) throw new IllegalStateException("Pairing is not active.");
        if (mCancellationRequested) {
            throw new IOException("Pairing cancelled.");
        }
        mActive = attempt;
    }

    T cancel() {
        if (mPending != null) {
            mCancellationRequested = true;
        }
        return mActive;
    }

    void unregister(Object pending, T attempt) {
        if (mPending == pending) {
            mPending = null;
            mCancellationRequested = false;
            if (mActive == attempt) mActive = null;
        }
    }
}
