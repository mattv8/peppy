package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlinx.coroutines.runBlocking
import java.util.concurrent.CancellationException

class ServerReachabilityTest {
    private val first = ReachabilityBinding("https://one.example", "vault", "device", 1)

    @Test fun bindingChangesResetTransientStateAndFenceStaleCallbacks() {
        val store = ReachabilityStore()
        store.bind(first)
        assertFalse(store.record(first, ServerReachability.UNAVAILABLE))
        assertEquals(ServerReachability.UNAVAILABLE, store.state.value)

        val replacement = first.copy(generation = 2)
        store.bind(replacement)
        assertEquals(ServerReachability.UNKNOWN, store.state.value)
        store.record(first, ServerReachability.UNAVAILABLE)
        assertEquals(ServerReachability.UNKNOWN, store.state.value)
    }

    @Test fun successfulResponseRecoveryIsReportedOnce() {
        val store = ReachabilityStore()
        store.bind(first)
        store.record(first, ServerReachability.UNAVAILABLE)

        assertTrue(store.record(first, ServerReachability.AVAILABLE))
        assertFalse(store.record(first, ServerReachability.AVAILABLE))
    }

    @Test fun recoveryCallbackStormQueuesOnlyOneImmediateProbe() = runBlocking {
        val requests = RecoveryProbeRequests()

        assertTrue(requests.request())
        assertFalse(requests.request())
        assertTrue(requests.waitForNext(100))
        assertTrue(requests.request())
    }

    @Test(expected = CancellationException::class)
    fun cancellationIsRethrownInsteadOfRecordingAnUnavailableServer() {
        isReachabilityFailure(CancellationException())
    }

    @Test fun foregroundProbeBackoffStartsAtThirtySecondsAndCapsAtFiveMinutes() {
        assertEquals(60_000L, nextProbeDelay(INITIAL_PROBE_DELAY_MS))
        assertEquals(120_000L, nextProbeDelay(60_000L))
        assertEquals(MAX_PROBE_DELAY_MS, nextProbeDelay(MAX_PROBE_DELAY_MS))
    }
}
