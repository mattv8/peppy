package io.github.muntashirakon.adb;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.EOFException;
import java.io.IOException;
import java.net.SocketException;
import java.net.SocketTimeoutException;

public class PairingConnectionCtxPeerInfoFailureTest {
    @Test
    public void decryptAuthenticationFailureIsAPairingCodeRejection() {
        assertTrue(PairingConnectionCtx.peerInfoDecryptionFailed()
                instanceof AdbPairingCodeRejectedException);
    }

    @Test
    public void peerCloseAfterSpakeIsAPairingCodeRejection() {
        IOException failure = PairingConnectionCtx.peerInfoReadFailure(new EOFException(), false);

        assertTrue(failure instanceof AdbPairingCodeRejectedException);
    }

    @Test
    public void localCancellationAndSocketFailuresRemainTransportErrors() {
        IOException locallyClosed = new EOFException();
        IOException timeout = new SocketTimeoutException();
        IOException socketFailure = new SocketException();

        assertSame(locallyClosed, PairingConnectionCtx.peerInfoReadFailure(locallyClosed, true));
        assertSame(timeout, PairingConnectionCtx.peerInfoReadFailure(timeout, false));
        assertSame(socketFailure, PairingConnectionCtx.peerInfoReadFailure(socketFailure, false));
        assertFalse(timeout instanceof AdbPairingCodeRejectedException);
        assertFalse(socketFailure instanceof AdbPairingCodeRejectedException);
    }
}
