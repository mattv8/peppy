package dev.peppy.mobile

import android.Manifest
import android.content.pm.PackageManager
import androidx.core.content.ContextCompat
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
import org.junit.Assert.assertTrue
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
        val readSmsGranted = ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) ==
            PackageManager.PERMISSION_GRANTED
        assertTrue("READ_SMS grant did not take effect", readSmsGranted)
        GatewayWork.captureHintPending(context)

        val result = TestListenableWorkerBuilder<GatewaySyncWorker>(context)
            .build()
            .startWork()
            .get(5, TimeUnit.SECONDS)

        assertEquals(ListenableWorker.Result.success(), result)
        val queued = WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get(5, TimeUnit.SECONDS)
        val schedulingState = "readSmsGranted=$readSmsGranted queued=${queued.map { "${it.id}:${it.state}" }}"
        assertEquals(schedulingState, 1, queued.size)
        assertEquals(schedulingState, WorkInfo.State.ENQUEUED, queued.single().state)
    }

    @After fun resetCaptureScheduling() {
        SmsCaptureWork.takeHintCount()
        GatewayWork.finishPass()
    }
}
