package dev.peppy.mobile.account

import androidx.credentials.exceptions.GetCredentialCancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Test

class HostedGoogleSignInTest {
    @Test fun credentialCancellationIsTheOnlySilentSignInResult() = runBlocking {
        val result = silentCredentialCancellation<String> {
            throw GetCredentialCancellationException("dismissed")
        }

        assertNull(result)
    }

    @Test fun genuineCredentialFailuresPropagate() = runBlocking {
        try {
            silentCredentialCancellation<String> { throw IllegalStateException("credential failure") }
            fail("Expected credential failure")
        } catch (failure: IllegalStateException) {
            assertEquals("credential failure", failure.message)
        }
    }
}
