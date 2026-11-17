package dev.peppy.mobile.gateway

import dev.peppy.mobile.LoopbackHttpServer
import java.io.IOException
import java.util.concurrent.CancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test


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

    @Test fun probeRequestsCapabilitiesAndNeverReplaysEvents() {
        val targets = mutableListOf<String>()
        LoopbackHttpServer { request ->
            targets += request.target
            LoopbackHttpServer.Response(200, "{\"capabilities\":[]}")
        }.use { server -> GatewayHttp(server.origin, "token").get(REACHABILITY_PROBE_PATH) }

        assertEquals(listOf("/v1/capabilities"), targets)
        assertEquals(GatewayHttpRoute.CAPABILITIES, GatewayHttp.routeCategory(REACHABILITY_PROBE_PATH))
    }

    @Test fun probeStatusClassesKeepTheirReachabilityMeaning() {
        val expected = mapOf(
            200 to ServerReachability.AVAILABLE,
            401 to ServerReachability.AVAILABLE,
            403 to ServerReachability.AVAILABLE,
            404 to ServerReachability.AVAILABLE,
            500 to ServerReachability.AVAILABLE,
            502 to ServerReachability.UNAVAILABLE,
            503 to ServerReachability.UNAVAILABLE,
            504 to ServerReachability.UNAVAILABLE,
        )
        for ((code, state) in expected) {
            val binding = probeBinding(code.toLong())
            ServerReachabilityState.bind(binding)
            ServerReachabilityState.recordResponse(binding, HttpResult(code, null))
            assertEquals("HTTP $code", state, ServerReachabilityState.state.value)
        }
    }

    @Test fun connectionFailureIsUnavailableButBudgetFailureIsNot() {
        val binding = probeBinding(1_000L)
        ServerReachabilityState.bind(binding)
        ServerReachabilityState.recordFailure(binding, GatewayTransportException("response exceeds budget"))
        assertEquals(ServerReachability.UNKNOWN, ServerReachabilityState.state.value)

        ServerReachabilityState.recordFailure(binding, IOException("connection reset"))
        assertEquals(ServerReachability.UNAVAILABLE, ServerReachabilityState.state.value)
    }

    private fun probeBinding(generation: Long) =
        GatewayReachabilityBinding("https://one.example", "vault", "device", "token", generation)
}
