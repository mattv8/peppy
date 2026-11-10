package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.impl.WorkManagerImpl
import androidx.work.impl.model.DependencyDao
import androidx.work.impl.model.WorkSpecDao
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import androidx.work.testing.WorkManagerTestInitHelper.ExecutorsMode
import androidx.work.workDataOf
import dev.peppy.mobile.mms.MmsCaptureHints
import dev.peppy.mobile.mms.MmsCaptureSummary
import java.util.UUID
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLooper

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsHintSchedulingTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private lateinit var executor: ExecutorService
    private lateinit var manager: WorkManagerImpl
    private lateinit var originalRunner: MmsCaptureRunner
    private lateinit var originalPhoneReadRunner: PhoneReadCaptureRunner
    private var originalPhoneReadLastRun: Long? = null

    @Before fun setUp() {
        originalRunner = MmsCaptureWork.runner
        originalPhoneReadRunner = PhoneReadCaptureWork.runner
        originalPhoneReadLastRun = PhoneReadCaptureWork.lastRunMs
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ -> summary() }
        PhoneReadCaptureWork.runner = PhoneReadCaptureRunner { false }
        MmsCaptureWork.hintScheduler.resetForTest()
        MmsCaptureHints.consumeProviderChange(context)
        executor = Executors.newSingleThreadExecutor()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(executor).setTaskExecutor(SynchronousExecutor()).build(),
            ExecutorsMode.PRESERVE_EXECUTORS,
        )
        manager = WorkManager.getInstance(context) as WorkManagerImpl
    }

    @After fun tearDown() {
        try {
            manager.cancelAllWork().result.get(5, TimeUnit.SECONDS)
            executor.shutdownNow()
            assertTrue(executor.awaitTermination(5, TimeUnit.SECONDS))
        } finally {
            MmsCaptureWork.runner = originalRunner
            PhoneReadCaptureWork.runner = originalPhoneReadRunner
            PhoneReadCaptureWork.lastRunMs = originalPhoneReadLastRun
            MmsCaptureWork.hintScheduler.resetForTest()
            MmsCaptureHints.consumeProviderChange(context)
            context.getSharedPreferences(MmsCaptureHints.PREFERENCES, Context.MODE_PRIVATE).edit().clear().commit()
            WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }

    @Test fun providerHintIsIndependentOfLongDelayedNormalWork() {
        val normal = enqueueDelayedNormal()

        MmsCaptureWork.enqueueProviderHint(context)

        val hint = uniqueWork(MmsCaptureWork.HINT_NAME).single()
        assertEquals(WorkInfo.State.ENQUEUED, hint.state)
        assertTrue(dependencyDao().getPrerequisites(hint.id.toString()).isEmpty())
        assertEquals(15_000L, workSpec(hint.id).initialDelay)
        assertTrue(workSpec(hint.id).input.getBoolean(MmsCaptureWork.HINT_WORK, false))
        assertNormalUnchanged(normal)
    }

    @Test fun providerHintPreservesQueuedManualRecheckInput() {
        val normal = enqueueDelayedNormal(manual = true)
        val inputBefore = workSpec(normal.id).input
        assertTrue(inputBefore.getBoolean(MmsCaptureWork.MANUAL_RECHECK, false))

        MmsCaptureWork.enqueueProviderHint(context)

        assertNormalUnchanged(normal)
        assertEquals(inputBefore, workSpec(normal.id).input)
        assertTrue(workSpec(normal.id).input.getBoolean(MmsCaptureWork.MANUAL_RECHECK, false))
        val hint = uniqueWork(MmsCaptureWork.HINT_NAME).single()
        assertEquals(WorkInfo.State.ENQUEUED, hint.state)
        assertTrue(dependencyDao().getPrerequisites(hint.id.toString()).isEmpty())
    }

    @Test fun settlingHintContinuesOnHintLaneAndCopyPassSchedulesGatewayDespitePendingNormal() {
        val normal = enqueueDelayedNormal()
        val passes = AtomicInteger()
        MmsCaptureWork.runner = MmsCaptureRunner { _, manual, providerChanged ->
            assertFalse(manual)
            when (passes.incrementAndGet()) {
                1 -> {
                    assertTrue(providerChanged)
                    summary(more = true)
                }
                2 -> summary(captured = 1)
                else -> throw AssertionError("unexpected extra capture pass")
            }
        }
        MmsCaptureWork.enqueueProviderHint(context)
        val first = uniqueWork(MmsCaptureWork.HINT_NAME).single()
        val driver = requireNotNull(WorkManagerTestInitHelper.getTestDriver(context))
        driver.setInitialDelayMet(first.id)
        await("first hint succeeds and leaves one delayed hint successor") {
            val hints = uniqueWork(MmsCaptureWork.HINT_NAME)
            hints.size == 2 && hints.single { it.id == first.id }.state == WorkInfo.State.SUCCEEDED &&
                hints.single { it.id != first.id }.state == WorkInfo.State.ENQUEUED
        }
        assertEquals(1, passes.get())
        assertNormalUnchanged(normal)
        assertTrue(uniqueWork(GatewayWork.NAME).isEmpty())
        val successor = uniqueWork(MmsCaptureWork.HINT_NAME).single { it.id != first.id }
        assertEquals(listOf(first.id.toString()), dependencyDao().getPrerequisites(successor.id.toString()))
        assertTrue(workSpec(successor.id).input.getBoolean(MmsCaptureWork.HINT_WORK, false))
        assertEquals(15_000L, workSpec(successor.id).initialDelay)

        driver.setInitialDelayMet(successor.id)
        await("copy hint succeeds and enqueues upload") {
            uniqueWork(MmsCaptureWork.HINT_NAME).all { it.state == WorkInfo.State.SUCCEEDED } &&
                uniqueWork(GatewayWork.NAME).size == 1
        }
        assertEquals(2, passes.get())
        assertEquals(2, uniqueWork(MmsCaptureWork.HINT_NAME).size)
        // Network constraints are never released: enqueue is tested without native gateway I/O.
        assertEquals(WorkInfo.State.ENQUEUED, uniqueWork(GatewayWork.NAME).single().state)
        assertNormalUnchanged(normal)
    }

    private fun enqueueDelayedNormal(manual: Boolean = false): WorkInfo {
        val request = OneTimeWorkRequestBuilder<MmsCaptureWorker>()
            .setInitialDelay(5, TimeUnit.HOURS)
            .setInputData(workDataOf(MmsCaptureWork.MANAGED_WORK to true, MmsCaptureWork.MANUAL_RECHECK to manual))
            .build()
        manager.enqueueUniqueWork(MmsCaptureWork.NAME, ExistingWorkPolicy.KEEP, request).result.get(5, TimeUnit.SECONDS)
        return uniqueWork(MmsCaptureWork.NAME).single().also {
            assertEquals(request.id, it.id)
            assertEquals(WorkInfo.State.ENQUEUED, it.state)
        }
    }

    private fun assertNormalUnchanged(before: WorkInfo) {
        val after = uniqueWork(MmsCaptureWork.NAME).single()
        assertEquals(before.id, after.id)
        assertEquals(before.state, after.state)
        assertEquals(0, after.runAttemptCount)
        assertEquals(TimeUnit.HOURS.toMillis(5), workSpec(after.id).initialDelay)
    }

    // Room is a runtime-only transitive dependency here; inspect the real WorkDatabase
    // through reflection rather than adding a production compile dependency for tests.
    private fun databaseDao(name: String): Any {
        val database = requireNotNull(manager.javaClass.getMethod("getWorkDatabase").invoke(manager))
        return requireNotNull(database.javaClass.getMethod(name).invoke(database))
    }

    private fun dependencyDao() = databaseDao("dependencyDao") as DependencyDao

    private fun workSpec(id: UUID) = requireNotNull((databaseDao("workSpecDao") as WorkSpecDao).getWorkSpec(id.toString()))

    private fun uniqueWork(name: String): List<WorkInfo> = manager.getWorkInfosForUniqueWork(name).get(5, TimeUnit.SECONDS)

    private fun summary(captured: Int = 0, more: Boolean = false) =
        MmsCaptureSummary(captured = captured, pending = 0, unavailable = 0, more = more)

    private fun await(description: String, condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (System.nanoTime() < deadline) {
            ShadowLooper.runUiThreadTasksIncludingDelayedTasks()
            if (condition()) return
            Thread.yield()
        }
        fail(description)
    }
}
