package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayWorkTest {
    @Test fun productionCoalescerAppendsOnlyOneSuccessorForAnActivePass() {
        val coalescer = GatewaySyncCoalescer()
        coalescer.begin()

        assertTrue(coalescer.request())
        assertFalse(coalescer.request())
        coalescer.finish()

        // A wake between finish and WorkManager observing success must not be lost to KEEP.
        val finishing = GatewaySyncCoalescer().apply { begin(); finish() }
        assertTrue(finishing.request())
        assertFalse(finishing.request())
    }

    @Test fun normalEnqueueCoalescesHintsBeforeAPassStarts() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )

        GatewayWork.enqueue(context)
        GatewayWork.enqueue(context)

        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
    }

    @Test fun captureFollowupDoesNotRequestAnotherCapturePass() {
        val context = testContext()
        GatewayWork.beginPass(context)

        GatewayWork.enqueueAfterCapture(context)

        assertFalse(GatewayWork.beginPass(context))
    }

    @Test fun ordinaryAndUrgentEnqueuesRequestCapture() {
        val context = testContext()

        GatewayWork.enqueue(context)
        assertTrue(GatewayWork.beginPass(context))
        GatewayWork.enqueueUrgent(context)

        assertTrue(GatewayWork.beginPass(context))
    }

    @Test fun captureHintIsClearedAfterAPass() {
        val context = testContext()

        GatewayWork.enqueue(context)

        assertTrue(GatewayWork.beginPass(context))
        assertFalse(GatewayWork.beginPass(context))
    }

    @Test fun deferredProducerPassRestoresConsumedCaptureHint() {
        val context = testContext()
        GatewayWork.enqueue(context)
        assertTrue(GatewayWork.beginPass(context))

        GatewayWork.restoreCaptureHint(context)

        assertTrue(GatewayWork.beginPass(context))
    }

    @Test fun missingCaptureHintConservativelyRequestsCapture() {
        val context = testContext()

        assertTrue(GatewayWork.beginPass(context))
    }

    @Test fun smsCaptureRequiresHintAndReadPermission() {
        assertTrue(GatewayWork.shouldEnqueueSmsCapture(captureHint = true, readGranted = true))
        assertFalse(GatewayWork.shouldEnqueueSmsCapture(captureHint = true, readGranted = false))
        assertFalse(GatewayWork.shouldEnqueueSmsCapture(captureHint = false, readGranted = true))
        assertFalse(GatewayWork.shouldEnqueueSmsCapture(captureHint = false, readGranted = false))
    }

    @Test fun mmsCaptureRequiresHintEnablementAndReceivePermission() {
        assertTrue(GatewayWork.shouldEnqueueMmsCapture(captureHint = true, enabled = true, receiveGranted = true))
        assertFalse(GatewayWork.shouldEnqueueMmsCapture(captureHint = true, enabled = true, receiveGranted = false))
        assertFalse(GatewayWork.shouldEnqueueMmsCapture(captureHint = true, enabled = false, receiveGranted = true))
        assertFalse(GatewayWork.shouldEnqueueMmsCapture(captureHint = false, enabled = true, receiveGranted = true))
    }

    @Test fun urgentWorkUsesExpeditedFallbackOnlyOnSupportedAndroidVersions() {
        assertFalse(GatewayWork.createRequest(urgent = true, apiLevel = 30).workSpec.expedited)
        assertTrue(GatewayWork.createRequest(urgent = true, apiLevel = 31).workSpec.expedited)
    }

    @After fun finishCoalescedPass() {
        GatewayWork.finishPass()
    }

    private fun testContext(): Context {
        val context = ApplicationProvider.getApplicationContext<Context>()
        GatewayWork.finishPass()
        context.getSharedPreferences("peppy-gateway-sync-coalesce", Context.MODE_PRIVATE).edit().clear().commit()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        return context
    }
}
