package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import androidx.work.testing.WorkManagerTestInitHelper.ExecutorsMode
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLooper

/** Real WorkManager scheduling for MMS continuations, following [SmsCaptureSchedulingTest]. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsCaptureSchedulingTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private lateinit var workerExecutor: ExecutorService
    private lateinit var productionRunner: MmsCaptureRunner

    @Before fun setUpWorkManager() {
        productionRunner = MmsCaptureWork.runner
        MmsCaptureWork.hintScheduler.resetForTest()
        workerExecutor = Executors.newSingleThreadExecutor()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder()
                .setExecutor(workerExecutor)
                .setTaskExecutor(SynchronousExecutor())
                .build(),
            ExecutorsMode.PRESERVE_EXECUTORS,
        )
    }

    @After fun tearDownWorkManager() {
        MmsCaptureWork.runner = productionRunner
        MmsCaptureWork.hintScheduler.resetForTest()
        WorkManagerTestInitHelper.closeWorkDatabase()
        workerExecutor.shutdownNow()
        assertTrue(workerExecutor.awaitTermination(1, TimeUnit.SECONDS))
    }

    @Test fun morePassesAppendContinuationsBehindTheRunningWorkerUntilAPassCompletes() {
        val passes = AtomicInteger()
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ -> summary(more = passes.incrementAndGet() < 3) }

        val completed = runDelayedPassToTerminal(expectedCount = 3)

        assertEquals(3, passes.get())
        assertEquals(3, completed.size)
        assertTrue(completed.all { it.state == WorkInfo.State.SUCCEEDED })
    }

    @Test fun completePassAppendsNoContinuation() {
        val passes = AtomicInteger()
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ -> passes.incrementAndGet(); summary(more = false) }

        val completed = runDelayedPassToTerminal(expectedCount = 1)

        assertEquals(1, passes.get())
        assertEquals(WorkInfo.State.SUCCEEDED, completed.single().state)
    }

    @Test fun transientPassFailureRetriesWithoutAppendingAContinuation() {
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ -> throw IOException("provider unavailable") }

        val result = TestListenableWorkerBuilder<MmsCaptureWorker>(context).build().startWork().get(5, TimeUnit.SECONDS)

        assertEquals(ListenableWorker.Result.retry(), result)
        assertTrue(uniqueWork().isEmpty())
    }

    @Test fun cancelledWorkerStaysCancelledAndReleasesThePassLock() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ ->
            entered.countDown()
            assertTrue(release.await(5, TimeUnit.SECONDS))
            summary(more = true)
        }
        val cancelled = TestListenableWorkerBuilder<MmsCaptureWorker>(context).build().startWork()
        assertTrue(entered.await(5, TimeUnit.SECONDS))

        cancelled.cancel(true)
        release.countDown()
        MmsCaptureWork.runner = MmsCaptureRunner { _, _, _ -> summary(more = false) }
        val next = TestListenableWorkerBuilder<MmsCaptureWorker>(context).build().startWork().get(5, TimeUnit.SECONDS)

        assertTrue(cancelled.isCancelled)
        assertEquals(ListenableWorker.Result.success(), next)
        assertTrue(uniqueWork().isEmpty())
    }

    private fun summary(more: Boolean) = MmsCaptureSummary(captured = 0, pending = 0, unavailable = 0, more = more)

    private fun uniqueWork(): List<WorkInfo> =
        WorkManager.getInstance(context).getWorkInfosForUniqueWork(MmsCaptureWork.NAME).get(1, TimeUnit.SECONDS)

    /** Starts the production delayed request, lifts its delay, and waits for every chained entry to finish. */
    private fun runDelayedPassToTerminal(expectedCount: Int): List<WorkInfo> {
        MmsCaptureWork.enqueue(context)
        val driver = requireNotNull(WorkManagerTestInitHelper.getTestDriver(context))
        val released = mutableSetOf<java.util.UUID>()
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        var latest = emptyList<WorkInfo>()
        while (System.nanoTime() < deadline) {
            ShadowLooper.runUiThreadTasksIncludingDelayedTasks()
            latest = uniqueWork()
            latest.filter { it.state == WorkInfo.State.ENQUEUED && released.add(it.id) }
                .forEach { driver.setInitialDelayMet(it.id) }
            if (latest.size > expectedCount) fail("unexpected continuation loop: $latest")
            if (latest.size == expectedCount && latest.all { it.state.isFinished }) return latest
            Thread.yield()
        }
        fail("unique work did not reach $expectedCount terminal entries: $latest")
        throw AssertionError("unreachable")
    }
}
