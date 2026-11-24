package dev.peppy.mobile

import android.app.Application
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import com.google.firebase.messaging.RemoteMessage
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RemoteMessagingProtocolTest {
    @Test fun onlyKnownBoundedServerHintsTriggerSync() {
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"ready\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("""{"frame_types":["event"],"high_water_cursor":"43","protocol_version":1,"replay_floor_cursor":"1","resume_cursor":"42","type":"ready"}"""))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"event\",\"cursor\":\"43\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"resync_required\"}"))
        assertFalse(RemoteMessagingProtocol.shouldSync("{\"type\":\"unknown\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"event\",\"envelope\":\"" + "x".repeat(70_000) + "\"}"))
        assertFalse(RemoteMessagingProtocol.shouldSync("x".repeat(RemoteMessagingProtocol.MAX_FRAME_BYTES + 1)))
    }

    @Test fun ownEventDetectionOnlyMatchesSmallEventFramesFromThisDevice() {
        val deviceId = "123e4567-e89b-12d3-a456-426614174000"
        val ownEvent = """{"type":"event","envelope":{"producer_device_id":"$deviceId"}}"""

        assertTrue("own producer", RemoteMessagingProtocol.isOwnEvent(ownEvent, deviceId))
        assertTrue("upper-case device ID", RemoteMessagingProtocol.isOwnEvent(ownEvent, deviceId.uppercase()))
        assertTrue("upper-case envelope ID", RemoteMessagingProtocol.isOwnEvent(ownEvent.replace(deviceId, deviceId.uppercase()), deviceId))
        assertFalse("other producer", RemoteMessagingProtocol.isOwnEvent(ownEvent.replace(deviceId, "123e4567-e89b-12d3-a456-426614174001"), deviceId))
        assertFalse("large event", RemoteMessagingProtocol.isOwnEvent("""{"type":"event","envelope":{"producer_device_id":"$deviceId"},"padding":"${"x".repeat(70_000)}"}""", deviceId))
        assertFalse("malformed JSON", RemoteMessagingProtocol.isOwnEvent("{", deviceId))
        assertFalse("missing envelope", RemoteMessagingProtocol.isOwnEvent("""{"type":"event"}""", deviceId))
        assertFalse("ready frame", RemoteMessagingProtocol.isOwnEvent("""{"type":"ready","envelope":{"producer_device_id":"$deviceId"}}""", deviceId))
        assertFalse("resync frame", RemoteMessagingProtocol.isOwnEvent("""{"type":"resync_required","envelope":{"producer_device_id":"$deviceId"}}""", deviceId))
        assertFalse("blank device ID", RemoteMessagingProtocol.isOwnEvent(ownEvent, ""))
    }

    @Test fun helloRequestsAdaptiveKeepaliveAlongsideVersionAndCursor() {
        val hello = JSONObject(RemoteMessagingProtocol.hello("42"))
        assertEquals("adaptive", hello.getString("keepalive"))
        assertEquals(1, hello.getInt("protocol_version"))
        assertEquals("42", hello.getString("resume_cursor"))
    }

    @Test fun keepaliveReadsReadyObjectAndKeepaliveFrames() {
        assertEquals(RemoteMessagingProtocol.Keepalive(20_000, true), RemoteMessagingProtocol.keepalive("""{"type":"ready","keepalive":{"interval_ms":20000,"peers":true}}"""))
        assertEquals(RemoteMessagingProtocol.Keepalive(50_000, false), RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":50000,"peers":false}"""))
    }

    @Test fun keepaliveIsNullWhenAbsentMalformedOrOutOfBounds() {
        assertNull("ready without keepalive", RemoteMessagingProtocol.keepalive("""{"type":"ready","frame_types":["event"],"protocol_version":1}"""))
        assertNull("event frame", RemoteMessagingProtocol.keepalive("""{"type":"event","keepalive":{"interval_ms":20000,"peers":true}}"""))
        assertNull("missing peers", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":20000}"""))
        assertNull("string peers", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":20000,"peers":"true"}"""))
        assertNull("string interval", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":"20000","peers":true}"""))
        assertNull("interval 4999", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":4999,"peers":true}"""))
        assertEquals(RemoteMessagingProtocol.Keepalive(5_000, true), RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":5000,"peers":true}"""))
        assertEquals(RemoteMessagingProtocol.Keepalive(600_000, true), RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":600000,"peers":true}"""))
        assertNull("interval 600001", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":600001,"peers":true}"""))
        assertNull("malformed JSON", RemoteMessagingProtocol.keepalive("{"))
        assertNull("oversized frame", RemoteMessagingProtocol.keepalive("""{"type":"keepalive","interval_ms":20000,"peers":true,"padding":"${"x".repeat(70_000)}"}"""))
    }

    @Test fun keepaliveFramesAreNotSyncHints() {
        assertFalse(RemoteMessagingProtocol.shouldSync("""{"type":"keepalive","interval_ms":20000,"peers":true}"""))
    }

    @Test fun reconnectDelayIsBoundedAndJittered() {
        assertEquals(1_000L, RemoteMessagingBackoff.delayMs(0, 0.0))
        assertEquals(30_000L, RemoteMessagingBackoff.delayMs(99, 0.0))
        assertTrue(RemoteMessagingBackoff.delayMs(3, 0.5) > RemoteMessagingBackoff.delayMs(3, 0.0))
    }

    @Test fun staleCallbacksAreRejectedAndAnEligibleRestartGetsANewGeneration() {
        val fence = RemoteMessagingFence()
        val first = fence.next()
        assertTrue(fence.accepts(first))
        val second = fence.next()
        assertFalse(fence.accepts(first))
        assertTrue(fence.accepts(second))
        fence.stop()
        assertFalse(fence.accepts(second))
        val restarted = fence.start()!!
        assertFalse(fence.accepts(second))
        assertTrue(fence.accepts(restarted))
        fence.destroy()
        assertEquals(null, fence.start())
    }

    @Test fun wakeAlwaysEnqueuesSyncAndRequestsForegroundStartOnlyWhenEnabled() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        val shadow = Shadows.shadowOf(context as Application)

        // Disabled wake still queues bounded sync and must not start the FGS.
        RemoteMessagingPreferences.setEnabled(context, false)
        RemoteMessagingService.resumeOrEnqueue(context, GatewayEnqueueCause.RELAY_WAKE)
        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
        assertNull(shadow.nextStartedService)

        // Enabled wake requests the FGS without any caller-thread session check; the service decides eligibility.
        RemoteMessagingPreferences.setEnabled(context, true)
        RemoteMessagingService.resumeOrEnqueue(context, GatewayEnqueueCause.RELAY_WAKE)
        assertEquals(RemoteMessagingService::class.java.name, shadow.nextStartedService.component?.className)
    }

    @Test fun acceptedFcmWakeStillQueuesSyncWhenLiveSocketIsDisabled() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        val shadow = Shadows.shadowOf(context as Application)
        val previousWakeEnabled = RelayWakeRuntime.enabled
        try {
            RelayWakeRuntime.enabled = true
            RemoteMessagingPreferences.setEnabled(context, false)

            org.robolectric.Robolectric.buildService(PeppyMessagingService::class.java).create().get()
                .onMessageReceived(RemoteMessage.Builder("123456789@gcm.googleapis.com").addData("kind", "wake").build())

            assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
            assertNull(shadow.nextStartedService)
        } finally {
            RelayWakeRuntime.enabled = previousWakeEnabled
        }
    }
}
