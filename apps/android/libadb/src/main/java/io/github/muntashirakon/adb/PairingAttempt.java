// SPDX-License-Identifier: GPL-3.0-or-later OR Apache-2.0

package io.github.muntashirakon.adb;

import java.io.IOException;

interface PairingAttempt extends AutoCloseable {
    void start() throws IOException;

    @Override
    void close();
}
