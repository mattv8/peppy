package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeGatewayHostFacts
import uniffi.peppy_mobile_bindings.NativeGatewayPlatform

class NotificationDismissalStatusTest {
    private val facts = NativeGatewayHostFacts(
        wifiConnected = true,
        notificationListenerAvailable = false,
        smsAvailable = true,
        notificationIsSilent = false,
        phoneReadAvailable = true,
        notificationMarkReadAvailable = true,
    )

    @Test
    fun publishesAndroidFactsAtTheCurrentTime() {
        val core = RecordingCore()
        val publisher = NotificationDismissalStatus(core, nowMs = { 42L })

        assertTrue(publisher.publish(facts))

        assertEquals(listOf(Publish(NativeGatewayPlatform.ANDROID, facts, 42L)), core.publishes)
    }

    @Test
    fun lockedKeysOrClosedClientReadAsNotQueuedAndAreNotRetried() {
        for (failure in listOf(MobileBindingsException.KeysUnavailable(), MobileBindingsException.Closed())) {
            val core = RecordingCore(failure = failure)
            val publisher = NotificationDismissalStatus(core, nowMs = { 7L })

            assertFalse(publisher.publish(facts))

            assertEquals(1, core.publishes.size)
        }
    }

    @Test
    fun unexpectedCoreFailurePropagatesSoTheGatewayPhaseRecordsIt() {
        val core = RecordingCore(failure = MobileBindingsException.Database())
        val publisher = NotificationDismissalStatus(core, nowMs = { 7L })

        assertThrows(MobileBindingsException.Database::class.java) { publisher.publish(facts) }

        assertEquals(1, core.publishes.size)
    }

    @Test
    fun coreDecisionIsReturnedUnchanged() {
        assertFalse(NotificationDismissalStatus(RecordingCore(queued = false), nowMs = { 1L }).publish(facts))
    }

    private data class Publish(val platform: NativeGatewayPlatform, val facts: NativeGatewayHostFacts, val nowMs: Long)

    private class RecordingCore(
        private val failure: MobileBindingsException? = null,
        private val queued: Boolean = true,
    ) : NotificationDismissalStatusCore {
        val publishes = mutableListOf<Publish>()

        override fun publish(platform: NativeGatewayPlatform, facts: NativeGatewayHostFacts, nowMs: Long): Boolean {
            publishes += Publish(platform, facts, nowMs)
            failure?.let { throw it }
            return queued
        }
    }
}
