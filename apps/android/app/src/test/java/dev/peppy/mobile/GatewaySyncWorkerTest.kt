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
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.ImportResult
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.*
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

    @Test fun periodicPassWithoutCaptureHintDoesNotQueueCaptureAndEnsuresPeriodicWork() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        assertEquals(ImportResult.IMPORTED, enroll())
        assertNull(NativeGateway.session(context))
        grant(Manifest.permission.READ_SMS)
        GatewayWork.ensurePeriodic(context)
        val workManager = WorkManager.getInstance(context)
        val periodicGateway = workManager.getWorkInfosForUniqueWork(GatewayWork.PERIODIC_NAME)
            .get(5, TimeUnit.SECONDS)
            .single()
        GatewayWork.beginPass(context)

        val result = TestListenableWorkerBuilder<GatewaySyncWorker>(context)
            .setId(periodicGateway.id)
            .build()
            .startWork()
            .get(5, TimeUnit.SECONDS)

        assertEquals(ListenableWorker.Result.success(), result)
        val queuedSms = workManager.getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get(5, TimeUnit.SECONDS)
        val queuedMms = workManager.getWorkInfosForUniqueWork(MmsCaptureWork.NAME).get(5, TimeUnit.SECONDS)
        val periodicSms = workManager.getWorkInfosForUniqueWork(SmsCaptureWork.PERIODIC_NAME).get(5, TimeUnit.SECONDS)
        val periodicMms = workManager.getWorkInfosForUniqueWork(MmsCaptureWork.PERIODIC_NAME).get(5, TimeUnit.SECONDS)
        val schedulingState = "queuedSms=$queuedSms queuedMms=$queuedMms periodicSms=$periodicSms periodicMms=$periodicMms"
        assertTrue(schedulingState, queuedSms.isEmpty())
        assertTrue(schedulingState, queuedMms.isEmpty())
        assertEquals(schedulingState, 1, periodicSms.size)
        assertEquals(schedulingState, 1, periodicMms.size)
    }

    @After fun resetCaptureScheduling() {
        SmsCaptureWork.takeHintCount()
        GatewayWork.finishPass()
    }
}
