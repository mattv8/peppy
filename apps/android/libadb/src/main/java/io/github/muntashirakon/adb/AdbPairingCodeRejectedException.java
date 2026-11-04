// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import java.io.IOException;

/** Indicates that the peer rejected pairing because the pairing codes do not match. */
public class AdbPairingCodeRejectedException extends IOException {
    public AdbPairingCodeRejectedException() {
        super("Pairing code was rejected.");
    }
}
