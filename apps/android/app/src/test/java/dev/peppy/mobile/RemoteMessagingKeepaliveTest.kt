package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RemoteMessagingKeepaliveTest {
    private val fast = RemoteMessagingProtocol.Keepalive(20_000, true)
    private val idle = RemoteMessagingProtocol.Keepalive(50_000, false)

    @Test fun silenceLimitIsTwoIntervalsPlusTenSecondsOrTheHandshakeLimit() {
        assertEquals(50_000L, RemoteMessagingLiveness.silenceLimitMs(fast))
        assertEquals(110_000L, RemoteMessagingLiveness.silenceLimitMs(idle))
        assertEquals(30_000L, RemoteMessagingLiveness.silenceLimitMs(null))
    }

    @Test fun checkCadenceFollowsTheAdvertisedIntervalOrTenSecondsWhenUnknown() {
        assertEquals(20_000L, RemoteMessagingLiveness.checkEveryMs(fast))
        assertEquals(50_000L, RemoteMessagingLiveness.checkEveryMs(idle))
        assertEquals(10_000L, RemoteMessagingLiveness.checkEveryMs(null))
    }

    @Test fun staleIsReachedExactlyAtTheSilenceLimit() {
        assertFalse(RemoteMessagingLiveness.isStale(1_000, 50_999, fast))
        assertTrue(RemoteMessagingLiveness.isStale(1_000, 51_000, fast))
        assertFalse(RemoteMessagingLiveness.isStale(0, 29_999, null))
        assertTrue(RemoteMessagingLiveness.isStale(0, 30_000, null))
    }

    @Test fun aSocketThatDiesAfterAFrameIsDetectedAtTheSilenceLimitNotACadenceLater() {
        // Connect at 0 s, ready at 1 s, keepalives at 21 s and 41 s, then silence.
        val frames = listOf(1_000L, 21_000L, 41_000L)
        var now = 0L
        var keepalive: RemoteMessagingProtocol.Keepalive? = null
        var last = 0L
        while (true) {
            now += RemoteMessagingLiveness.nextCheckDelayMs(last, now, keepalive)
            frames.lastOrNull { it <= now }?.let { last = it; keepalive = fast }
            if (RemoteMessagingLiveness.isStale(last, now, keepalive)) break
        }
        assertEquals(41_000L + 50_000L, now)
    }

    @Test fun nextCheckNeverWaitsLongerThanOneCadenceAndNeverLessThanAMillisecond() {
        assertEquals(20_000L, RemoteMessagingLiveness.nextCheckDelayMs(0, 0, fast))
        assertEquals(5_000L, RemoteMessagingLiveness.nextCheckDelayMs(0, 45_000, fast))
        assertEquals(1L, RemoteMessagingLiveness.nextCheckDelayMs(0, 60_000, fast))
        assertEquals(10_000L, RemoteMessagingLiveness.nextCheckDelayMs(0, 0, null))
    }

    @Test fun peerGateIsUnknownBeforeAnyKeepalive() {
        val gate = PeerPresenceGate()
        assertNull(gate.value(0))
        assertNull(gate.remainingGraceMs(0))
    }

    @Test fun absentPeersStayUnknownDuringTheGraceThenReportFalse() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(false, 0)
        assertNull(gate.value(0))
        assertNull(gate.value(59_999))
        assertEquals(false, gate.value(60_000))
        assertNull(gate.remainingGraceMs(60_000))
    }

    @Test fun presentPeersReportTrueAtOnce() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(true, 0)
        assertEquals(true, gate.value(0))
        assertNull(gate.remainingGraceMs(0))
    }

    @Test fun trueThenFalseStaysTrueDuringTheGrace() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(true, 1_000)
        gate.onKeepalive(false, 1_000)
        assertEquals(true, gate.value(1_000))
        assertEquals(true, gate.value(60_999))
        assertEquals(false, gate.value(61_000))
    }

    @Test fun flapInsideTheGraceNeverReportsFalseUntilSixtySecondsAfterTheLastFalseRunBegan() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(false, 0)
        gate.onKeepalive(true, 30_000)
        gate.onKeepalive(false, 40_000)
        assertTrue(gate.value(99_999) != false)
        assertEquals(false, gate.value(100_000))
    }

    @Test fun repeatedFalseFramesDoNotRestartTheGrace() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(false, 0)
        gate.onKeepalive(false, 20_000)
        gate.onKeepalive(false, 40_000)
        assertEquals(40_000L, gate.remainingGraceMs(20_000))
        assertEquals(false, gate.value(60_000))
    }

    @Test fun resetReturnsTheGateToUnknown() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(false, 0)
        assertEquals(false, gate.value(60_000))
        gate.reset()
        assertNull(gate.value(60_000))
        assertNull(gate.remainingGraceMs(60_000))
    }

    @Test fun remainingGraceCountsDownOnlyWhileFalseIsPending() {
        val gate = PeerPresenceGate()
        gate.onKeepalive(true, 1_000)
        gate.onKeepalive(false, 1_000)
        assertEquals(60_000L, gate.remainingGraceMs(1_000))
        assertEquals(30_000L, gate.remainingGraceMs(31_000))
        assertNull(gate.remainingGraceMs(61_000))
    }

    @Test fun preferencesRememberTheCapabilityForOneOriginAtATime() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val a = "https://a.example"
        val b = "https://b.example"
        assertFalse(RemoteMessagingPreferences.adaptiveKeepalive(context, a))

        RemoteMessagingPreferences.setAdaptiveKeepalive(context, a, true)
        assertTrue(RemoteMessagingPreferences.adaptiveKeepalive(context, a))
        assertFalse(RemoteMessagingPreferences.adaptiveKeepalive(context, b))

        RemoteMessagingPreferences.setAdaptiveKeepalive(context, b, true)
        assertFalse(RemoteMessagingPreferences.adaptiveKeepalive(context, a))
        assertTrue(RemoteMessagingPreferences.adaptiveKeepalive(context, b))

        RemoteMessagingPreferences.setAdaptiveKeepalive(context, a, false)
        assertTrue("clearing another origin keeps the stored one", RemoteMessagingPreferences.adaptiveKeepalive(context, b))

        RemoteMessagingPreferences.setAdaptiveKeepalive(context, b, false)
        assertFalse(RemoteMessagingPreferences.adaptiveKeepalive(context, b))
    }
}
