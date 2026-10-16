package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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

    @Test fun urgentWorkUsesExpeditedFallbackOnlyOnSupportedAndroidVersions() {
        assertFalse(GatewayWork.createRequest(urgent = true, apiLevel = 30).workSpec.expedited)
        assertTrue(GatewayWork.createRequest(urgent = true, apiLevel = 31).workSpec.expedited)
    }
}
