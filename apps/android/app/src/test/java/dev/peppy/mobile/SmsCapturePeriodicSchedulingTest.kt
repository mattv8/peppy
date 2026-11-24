package dev.peppy.mobile

import androidx.test.core.app.ApplicationProvider
import android.content.Context
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.WorkerFactory
import androidx.work.WorkerParameters
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.SettableFuture
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import dev.peppy.mobile.diagnostics.SyncTraceField
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsCapturePeriodicSchedulingTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val entries = AtomicInteger()
    private val entered = CountDownLatch(1)
    private val release = CountDownLatch(1)
    private var previousRunner: SmsCaptureRunner? = null
    private lateinit var previousPhoneRead: PhoneReadCaptureRunner
    private var firstMore = false
    private val holdFirst = AtomicBoolean(false)
    private val firstResultReady = CountDownLatch(1)
    private val releaseFirstResult = CountDownLatch(1)
    private val periodicEntered = CountDownLatch(1)
    private val periodicRelease = CountDownLatch(1)

    @Before fun setUp() {
        val factory = object : WorkerFactory() {
            override fun createWorker(appContext: Context, workerClassName: String, workerParameters: WorkerParameters): ListenableWorker? {
                if (workerClassName != SmsCaptureWorker::class.java.name) return null
                val delegate = SmsCaptureWorker(appContext, workerParameters)
                return object : ListenableWorker(appContext, workerParameters) {
                    override fun startWork(): ListenableFuture<Result> {
                        val result = delegate.startWork()
                        if (!holdFirst.compareAndSet(true, false)) return result
                        val held = SettableFuture.create<Result>()
                        result.addListener({
                            try {
                                val completed = result.get()
                                firstResultReady.countDown()
                                check(releaseFirstResult.await(60, TimeUnit.SECONDS))
                                held.set(completed)
                            } catch (error: Exception) {
                                held.setException(error)
                            }
                        }, { task -> Thread(task, "held-sms-result").start() })
                        return held
                    }
                }
            }
        }
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context, Configuration.Builder().setExecutor(SynchronousExecutor()).setWorkerFactory(factory).build(),
        )
        SmsCaptureWork.takeHintCount()
        previousRunner = SmsCaptureWork.runner
        previousPhoneRead = PhoneReadCaptureWork.runner
        PhoneReadCaptureWork.runner = PhoneReadCaptureRunner { false }
        SmsCaptureWork.runner = SmsCaptureRunner {
            val entry = entries.incrementAndGet()
            if (entry == 1) {
                entered.countDown()
                check(release.await(60, TimeUnit.SECONDS))
            }
            SmsCaptureSummary(more = entry == 1 && firstMore)
        }
    }

    @After fun tearDown() {
        release.countDown()
        periodicRelease.countDown()
        releaseFirstResult.countDown()
        try {
            WorkManager.getInstance(context).cancelAllWork().result.get(60, TimeUnit.SECONDS)
        } finally {
            SmsCaptureWork.runner = previousRunner
            PhoneReadCaptureWork.runner = previousPhoneRead
            SmsCaptureWork.takeHintCount()
            WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }

    private fun work() = WorkManager.getInstance(context)
        .getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get(60, TimeUnit.SECONDS)

    private fun awaitState(description: String, predicate: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(60)
        while (!predicate()) {
            check(System.nanoTime() < deadline) { "$description: entries=${entries.get()} work=${work()}" }
            Thread.sleep(10)
        }
    }

    private fun awaitPeriodicCompletion() {
        awaitState("periodic completion") {
            WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.PERIODIC_NAME)
                .get(60, TimeUnit.SECONDS).single().state == WorkInfo.State.ENQUEUED
        }
    }

    @Test fun periodicPassOverlappingAHintRunsOneFollowUp() {
        SmsCaptureWork.ensurePeriodic(context)
        assertTrue(entered.await(60, TimeUnit.SECONDS))
        SmsCaptureWork.enqueue(context)
        release.countDown()
        awaitPeriodicCompletion()
        val blockedBeforeFollowUp = work().any { it.state == WorkInfo.State.BLOCKED }
        val delayed = work().first { it.state == WorkInfo.State.ENQUEUED }
        WorkManagerTestInitHelper.getTestDriver(context)!!.setInitialDelayMet(delayed.id)
        awaitState("follow-up completion") { work().all { it.state.isFinished } }
        assertEquals("periodic and one follow-up", 2, entries.get())
        assertFalse(blockedBeforeFollowUp)
    }

    @Test fun oneTimePassOverlappingAHintRunsOneFollowUp() {
        SmsCaptureWork.enqueue(context)
        WorkManagerTestInitHelper.getTestDriver(context)!!.setInitialDelayMet(work().single().id)
        assertTrue(entered.await(60, TimeUnit.SECONDS))
        SmsCaptureWork.enqueue(context)
        release.countDown()
        awaitState("one-time follow-up") { entries.get() == 2 && work().all { it.state.isFinished } }
        assertEquals(2, entries.get())
    }

    @Test fun periodicPassOverlappingObserverHintRunsOneFollowUp() {
        SmsCaptureWork.ensurePeriodic(context)
        assertTrue(entered.await(60, TimeUnit.SECONDS))
        SmsCaptureWork.enqueueObserver(context)
        awaitState("observer waiting") { SmsCaptureWork.hasWaitingOneTimeRun() }
        release.countDown()
        awaitPeriodicCompletion()
        awaitState("observer completion") { entries.get() == 2 && work().all { it.state.isFinished } }
        assertEquals(2, entries.get())
    }

    @Test fun cancellingParkedOneTimePassDoesNotSuppressPeriodicHintContinuation() {
        SmsCaptureWork.ensurePeriodic(context)
        assertTrue(entered.await(60, TimeUnit.SECONDS))
        SmsCaptureWork.enqueueObserver(context)
        awaitState("one-time pass parked") { SmsCaptureWork.hasWaitingOneTimeRun() }
        WorkManager.getInstance(context).cancelUniqueWork(SmsCaptureWork.NAME).result.get(60, TimeUnit.SECONDS)
        awaitState("cancelled waiter removed") { !SmsCaptureWork.hasWaitingOneTimeRun() }
        assertTrue(SmsCaptureWork.hasPendingHint())

        release.countDown()
        awaitPeriodicCompletion()
        awaitState("hint continuation completed") { entries.get() == 2 && work().all { it.state.isFinished } }
        assertEquals(2, entries.get())
    }

    @Test fun periodicPassWithMoreAndNoHintStillSchedulesAContinuation() {
        firstMore = true
        SmsCaptureWork.ensurePeriodic(context)
        assertTrue(entered.await(60, TimeUnit.SECONDS))
        assertTrue(work().isEmpty())
        release.countDown()
        awaitPeriodicCompletion()
        awaitState("backlog continuation") { entries.get() == 2 && work().isNotEmpty() && work().all { it.state.isFinished } }
        assertEquals(2, entries.get())
    }

    @Test fun periodicPassEnsuresAOneTimePassWhenTheHintHadNoRequest() {
        val events = Collections.synchronizedList(mutableListOf<String>())
        SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add) {
            SmsCaptureWork.ensurePeriodic(context)
            assertTrue(entered.await(60, TimeUnit.SECONDS))
            SmsCaptureWork.enqueue(context)
            WorkManager.getInstance(context).cancelUniqueWork(SmsCaptureWork.NAME).result.get(60, TimeUnit.SECONDS)
            assertTrue(SmsCaptureWork.hasPendingHint())
            assertTrue(work().none { !it.state.isFinished })
            release.countDown()
            awaitPeriodicCompletion()
            awaitState("ensured hint pass") { entries.get() == 2 && work().all { it.state.isFinished } }
        }
        assertEquals(2, entries.get())
        assertFalse(events.any {
            it.contains("event=${SyncTraceEvent.CAPTURE_START.code} ") &&
                it.contains(" ${SyncTraceField.COUNT.code}=1")
        })
    }

    @Test fun periodicHintAppendsBehindFinishedButRunningOneTimeWork() = finishingWindow(more = false)

    @Test fun periodicMoreAppendsBehindFinishedButRunningOneTimeWork() = finishingWindow(more = true)

    private fun finishingWindow(more: Boolean) {
        holdFirst.set(true)
        SmsCaptureWork.runner = SmsCaptureRunner {
            val entry = entries.incrementAndGet()
            when (entry) {
                2 -> {
                    periodicEntered.countDown()
                    check(periodicRelease.await(60, TimeUnit.SECONDS))
                }
            }
            SmsCaptureSummary(more = entry == 2 && more)
        }
        SmsCaptureWork.enqueue(context)
        WorkManagerTestInitHelper.getTestDriver(context)!!.setInitialDelayMet(work().single().id)
        assertTrue(firstResultReady.await(60, TimeUnit.SECONDS))
        assertEquals(WorkInfo.State.RUNNING, work().single().state)
        SmsCaptureWork.ensurePeriodic(context)
        assertTrue(periodicEntered.await(60, TimeUnit.SECONDS))
        if (!more) SmsCaptureWork.enqueue(context)
        periodicRelease.countDown()
        awaitPeriodicCompletion()
        assertTrue("periodic follow-up must append behind finished RUNNING work", work().any { it.state == WorkInfo.State.BLOCKED })
        releaseFirstResult.countDown()
        awaitState("appended follow-up") { entries.get() == 3 && work().all { it.state.isFinished } }
        assertEquals(3, entries.get())
    }
}
