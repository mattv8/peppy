package dev.peppy.mobile

import android.app.Application
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.json.JSONObject
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RemoteMessagingProtocolTest {
    @Test fun helloUsesTheCurrentCanonicalCursor() {
        val hello = JSONObject(RemoteMessagingProtocol.hello("42"))
        assertEquals(1, hello.getInt("protocol_version"))
        assertEquals("42", hello.getString("resume_cursor"))
    }

    @Test fun onlyKnownBoundedServerHintsTriggerSync() {
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"ready\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("""{"frame_types":["event"],"high_water_cursor":"43","protocol_version":1,"replay_floor_cursor":"1","resume_cursor":"42","type":"ready"}"""))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"event\",\"cursor\":\"43\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"resync_required\"}"))
        assertFalse(RemoteMessagingProtocol.shouldSync("{\"type\":\"unknown\"}"))
        assertTrue(RemoteMessagingProtocol.shouldSync("{\"type\":\"event\",\"envelope\":\"" + "x".repeat(70_000) + "\"}"))
        assertFalse(RemoteMessagingProtocol.shouldSync("x".repeat(RemoteMessagingProtocol.MAX_FRAME_BYTES + 1)))
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

    @Test fun activePassCoalescesDuplicateAndFinishingHintsIntoOneFollowUp() {
        val coalescer = GatewaySyncCoalescer()
        coalescer.begin()
        assertTrue(coalescer.request())
        assertFalse(coalescer.request())
        coalescer.finish()
        val afterFinish = GatewaySyncCoalescer().apply { begin(); finish() }
        assertTrue(afterFinish.request())
        assertFalse(afterFinish.request())
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
        RemoteMessagingService.resumeOrEnqueue(context)
        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
        assertNull(shadow.nextStartedService)

        // Enabled wake requests the FGS without any caller-thread session check; the service decides eligibility.
        RemoteMessagingPreferences.setEnabled(context, true)
        RemoteMessagingService.resumeOrEnqueue(context)
        assertEquals(RemoteMessagingService::class.java.name, shadow.nextStartedService.component?.className)
    }
}
