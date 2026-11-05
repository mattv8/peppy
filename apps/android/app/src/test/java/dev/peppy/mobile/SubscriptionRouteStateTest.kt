package dev.peppy.mobile

import dev.peppy.mobile.account.HostedFailure
import java.io.IOException
import java.security.ProviderException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.fail
import org.junit.Test

class SubscriptionRouteStateTest {
    private val selected = SubscriptionRouteState(
        selection = SubscriptionSelection("account", "vault", 4),
    )

    @Test fun openingReadsLocalIdentitiesWithoutChangingRevisionAndClearsPriorFeedback() = runBlocking {
        val opened = loadSubscriptionRoute(
            selected.copy(feedback = SubscriptionRouteFeedback.SIGN_IN),
            readAccountId = { "account" },
            readVaultId = { "vault" },
        )

        assertEquals(selected, opened)
    }

    @Test fun expiredLocalSessionRoutesToSignInWithoutFeedback() = runBlocking {
        val opened = loadSubscriptionRoute(
            selected,
            readAccountId = { throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED) },
            readVaultId = { "vault" },
        )

        assertEquals(SubscriptionSelection(null, "vault", 4), opened.selection)
        assertNull(opened.feedback)
    }

    @Test fun everyNonExpiryLocalReadFailureIsStorageFeedbackAndPreservesSelection() = runBlocking {
        listOf<Exception>(
            HostedFailure(HostedFailure.Kind.UNAVAILABLE),
            IOException("secure store unavailable"),
            IllegalArgumentException("corrupt session JSON"),
            ProviderException("keystore provider unavailable"),
        ).forEach { failure ->
            val opened = loadSubscriptionRoute(
                selected,
                readAccountId = { throw failure },
                readVaultId = { throw AssertionError("vault read must not follow a failed account read") },
            )

            assertEquals(selected.selection, opened.selection)
            assertEquals(SubscriptionRouteFeedback.STORAGE, opened.feedback)
        }
    }

    @Test fun vaultReadFailureIsStorageFeedbackAndPreservesSelection() = runBlocking {
        val opened = loadSubscriptionRoute(
            selected,
            readAccountId = { "new-account" },
            readVaultId = { throw IOException("vault identity unavailable") },
        )

        assertEquals(selected.selection, opened.selection)
        assertEquals(SubscriptionRouteFeedback.STORAGE, opened.feedback)
    }

    @Test fun openingRethrowsTrueCoroutineCancellation() = runBlocking {
        val cancellation = CancellationException("left route")

        try {
            loadSubscriptionRoute(selected, readAccountId = { throw cancellation }, readVaultId = { "vault" })
            fail("cancellation must propagate")
        } catch (actual: CancellationException) {
            assertSame(cancellation, actual)
        }
    }

    @Test fun dismissedCredentialUiIsSilentAndPreservesSelection() = runBlocking {
        val result = signInSubscriptionRoute(
            selected.copy(feedback = SubscriptionRouteFeedback.SIGN_IN),
            signIn = { null },
            readVaultId = { throw AssertionError("dismissal must not read the vault identity") },
        )

        assertEquals(selected, result)
    }

    @Test fun failedSignInPreservesSelectionWithTypedFeedback() = runBlocking {
        val result = signInSubscriptionRoute(
            selected,
            signIn = { throw IOException("provider failed") },
            readVaultId = { "new-vault" },
        )

        assertEquals(selected.selection, result.selection)
        assertEquals(SubscriptionRouteFeedback.SIGN_IN, result.feedback)
    }

    @Test fun failedVaultLookupAfterSignInPreservesSelectionWithSignInFeedback() = runBlocking {
        val result = signInSubscriptionRoute(
            selected,
            signIn = { "new-account" },
            readVaultId = { throw IOException("vault identity unavailable") },
        )

        assertEquals(selected.selection, result.selection)
        assertEquals(SubscriptionRouteFeedback.SIGN_IN, result.feedback)
    }

    @Test fun signInRethrowsTrueCoroutineCancellation() = runBlocking {
        val cancellation = CancellationException("activity stopped")

        try {
            signInSubscriptionRoute(selected, signIn = { throw cancellation }, readVaultId = { "vault" })
            fail("cancellation must propagate")
        } catch (actual: CancellationException) {
            assertSame(cancellation, actual)
        }
    }

    @Test fun sameAccountSignInAdvancesRevisionExactlyOnceAndClearsFeedback() = runBlocking {
        var signInCalls = 0
        var vaultReads = 0

        val signedIn = signInSubscriptionRoute(
            selected.copy(feedback = SubscriptionRouteFeedback.SIGN_IN),
            signIn = { signInCalls++; "account" },
            readVaultId = { vaultReads++; "new-vault" },
        )

        assertEquals(SubscriptionRouteState(SubscriptionSelection("account", "new-vault", 5)), signedIn)
        assertEquals(1, signInCalls)
        assertEquals(1, vaultReads)
    }
}
