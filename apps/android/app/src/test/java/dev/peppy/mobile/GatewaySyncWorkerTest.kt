package dev.peppy.mobile

import android.Manifest
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewaySyncWorkerTest : GatewayTestBase() {
    @Test fun lockedVaultWithoutNativeSessionQueuesSmsCaptureFromHint() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        assertEquals(ImportResult.IMPORTED, enroll())
        assertNull(NativeGateway.session(context))
        grant(Manifest.permission.READ_SMS)
        GatewayWork.captureHintPending(context)

        val result = TestListenableWorkerBuilder<GatewaySyncWorker>(context)
            .build()
            .startWork()
            .get(5, TimeUnit.SECONDS)

        assertEquals(ListenableWorker.Result.success(), result)
        val queued = WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get(5, TimeUnit.SECONDS)
        assertEquals(1, queued.size)
        assertEquals(WorkInfo.State.ENQUEUED, queued.single().state)
    }

    @After fun resetCaptureScheduling() {
        SmsCaptureWork.takeHintCount()
        GatewayWork.finishPass()
    }
}
