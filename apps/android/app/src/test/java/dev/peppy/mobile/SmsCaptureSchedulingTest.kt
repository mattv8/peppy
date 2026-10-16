package dev.peppy.mobile

import android.content.Context
import android.Manifest
import android.provider.Telephony
import androidx.test.core.app.ApplicationProvider
import androidx.work.BackoffPolicy
import androidx.work.Configuration
import androidx.work.Data
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import androidx.work.testing.WorkManagerTestInitHelper.ExecutorsMode
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLooper
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.robolectric.Shadows.shadowOf

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsCaptureSchedulingTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private lateinit var workerExecutor: ExecutorService

    @Before fun setUpWorkManager() {
        workerExecutor = Executors.newSingleThreadExecutor()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(workerExecutor).build(),
            ExecutorsMode.PRESERVE_EXECUTORS,
        )
        SmsCaptureWork.fastModeOverride = null
        SmsCaptureWork.runner = null
    }

    @After fun tearDownWorkManager() {
        WorkManagerTestInitHelper.closeWorkDatabase()
        workerExecutor.shutdownNow()
        assertTrue(workerExecutor.awaitTermination(1, TimeUnit.SECONDS))
        SmsCaptureWork.takeHintCount()
        SmsCaptureWork.fastModeOverride = null
        SmsCaptureWork.runner = null
    }

    @Test fun baselineDebounceUsesTrailingFifteenSecondQuietPeriod() {
        val debouncer = SmsCaptureDebouncer(smsCaptureDebouncePolicy(fastMode = false))

        assertEquals(15_000L, debouncer.nextDelayMs(0))
        assertEquals(15_000L, debouncer.nextDelayMs(5_000))
    }

    @Test fun fastDebounceCapsContinuousNotificationsAtThreeSeconds() {
        val debouncer = SmsCaptureDebouncer(smsCaptureDebouncePolicy(fastMode = true))

        assertEquals(1_000L, debouncer.nextDelayMs(0))
        assertEquals(1_000L, debouncer.nextDelayMs(1_500))
        assertEquals(500L, debouncer.nextDelayMs(2_500))
    }

    @Test fun resetStartsANewFastBurstWindow() {
        val debouncer = SmsCaptureDebouncer(smsCaptureDebouncePolicy(fastMode = true))

        debouncer.nextDelayMs(0)
        debouncer.reset()
        assertEquals(1_000L, debouncer.nextDelayMs(2_500))
    }

    @Test fun observerDelayIsFastOnlyForTheFastExperiment() {
        assertEquals(15_000L, smsCaptureInitialDelayMs(SmsCaptureRequestSource.DEFAULT, fastMode = false))
        assertEquals(15_000L, smsCaptureInitialDelayMs(SmsCaptureRequestSource.OBSERVER, fastMode = false))
        assertEquals(0L, smsCaptureInitialDelayMs(SmsCaptureRequestSource.OBSERVER, fastMode = true))
        assertEquals(15_000L, smsCaptureInitialDelayMs(SmsCaptureRequestSource.DEFAULT, fastMode = true))
        assertEquals(0L, smsCaptureInitialDelayMs(SmsCaptureRequestSource.CONTINUATION, fastMode = true))
    }

    @Test fun observerDispatchesOnceAfterQuietPeriodAndCapsAContinuousBurst() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context, smsCaptureDebouncePolicy(fastMode = true)) { dispatches += 1 }
        context.contentResolver.registerContentObserver(Telephony.Sms.CONTENT_URI, true, observer)

        ShadowLooper.idleMainLooper(3, TimeUnit.SECONDS)
        assertEquals(0, dispatches)
        context.contentResolver.notifyChange(Telephony.Sms.CONTENT_URI, null)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        context.contentResolver.notifyChange(Telephony.Sms.CONTENT_URI, null)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        context.contentResolver.notifyChange(Telephony.Sms.CONTENT_URI, null)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        context.contentResolver.notifyChange(Telephony.Sms.CONTENT_URI, null)
        ShadowLooper.idleMainLooper(300, TimeUnit.MILLISECONDS)

        assertEquals(1, dispatches)
        context.contentResolver.unregisterContentObserver(observer)
    }

    @Test fun baselineObserverDispatchesTwentySecondsAfterNotificationsAtZeroAndFiveSeconds() {
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context, smsCaptureDebouncePolicy(fastMode = false)) { dispatches++ }

        observer.onChange(false)
        ShadowLooper.idleMainLooper(5, TimeUnit.SECONDS)
        observer.onChange(false)
        ShadowLooper.idleMainLooper(14, TimeUnit.SECONDS)
        assertEquals(0, dispatches)
        ShadowLooper.idleMainLooper(1, TimeUnit.SECONDS)

        assertEquals(1, dispatches)
        observer.cancel()
    }

    @Test fun fastObserverDispatchesOneHintAfterOneSecond() {
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context, smsCaptureDebouncePolicy(fastMode = true)) { dispatches++ }

        observer.onChange(false)
        ShadowLooper.idleMainLooper(999, TimeUnit.MILLISECONDS)
        assertEquals(0, dispatches)
        ShadowLooper.idleMainLooper(1, TimeUnit.MILLISECONDS)

        assertEquals(1, dispatches)
        observer.cancel()
    }

    @Test fun unregisterCancelsPendingAndStaleObserverCallbacks() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context, smsCaptureDebouncePolicy(fastMode = true)) { dispatches += 1 }

        observer.onChange(false)
        observer.cancel()
        ShadowLooper.idleMainLooper(4, TimeUnit.SECONDS)
        observer.onChange(false)

        assertEquals(0, dispatches)
    }

    @Test fun requestFactoryPreservesDelaysAndExponentialBackoffForEveryMode() {
        val normal = SmsCaptureWork.createRequest(SmsCaptureRequestSource.DEFAULT, fastMode = true)
        val baselineObserver = SmsCaptureWork.createRequest(SmsCaptureRequestSource.OBSERVER, fastMode = false)
        val fastObserver = SmsCaptureWork.createRequest(SmsCaptureRequestSource.OBSERVER, fastMode = true)

        assertEquals(15_000L, normal.workSpec.initialDelay)
        assertEquals(15_000L, baselineObserver.workSpec.initialDelay)
        assertEquals(0L, fastObserver.workSpec.initialDelay)
        assertEquals(BackoffPolicy.EXPONENTIAL, fastObserver.workSpec.backoffPolicy)
        assertEquals(30_000L, fastObserver.workSpec.backoffDelayDuration)
        assertFalse(fastObserver.workSpec.expedited)
        assertTrue(normal.workSpec.id != fastObserver.workSpec.id)
    }

    @Test fun uniqueKeepPreservesDelayedDefaultRequestWhenFastObserverHintsArrive() {
        SmsCaptureWork.fastModeOverride = false
        SmsCaptureWork.enqueue(context)
        val manager = WorkManager.getInstance(context)
        val original = manager.getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get().single()

        SmsCaptureWork.fastModeOverride = true
        SmsCaptureWork.enqueueObserver(context)

        val retained = manager.getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get()
        assertEquals(1, retained.size)
        assertEquals(original.id, retained.single().id)
        assertEquals(androidx.work.WorkInfo.State.ENQUEUED, retained.single().state)
    }

    @Test fun workerAppendsOneContinuationForRunningHintThenNoOpPassTerminates() {
        val count = AtomicInteger()
        SmsCaptureWork.runner = SmsCaptureRunner {
            if (count.incrementAndGet() == 1) SmsCaptureWork.enqueueObserver(context)
            SmsCaptureSummary()
        }

        SmsCaptureWork.enqueue(context)
        val manager = WorkManager.getInstance(context)
        val request = manager.getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get().single()
        ShadowLooper.runUiThreadTasks()
        requireNotNull(WorkManagerTestInitHelper.getTestDriver(context)).setInitialDelayMet(request.id)
        val completed = awaitUniqueWorkTerminal(manager, expectedCount = 2)

        assertEquals(2, count.get())
        assertEquals(2, completed.size)
        assertTrue(completed.all { it.state == WorkInfo.State.SUCCEEDED })
        assertTrue(completed.none { it.state == WorkInfo.State.ENQUEUED || it.state == WorkInfo.State.RUNNING })
    }

    @Test fun noOpAndErrorWorkersDoNotAppendAndErrorsRetryWithExistingBackoffPolicy() {
        SmsCaptureWork.runner = SmsCaptureRunner { SmsCaptureSummary() }
        val noOp = TestListenableWorkerBuilder<SmsCaptureWorker>(context).build()
        assertEquals(androidx.work.ListenableWorker.Result.success(), noOp.startWork().get())
        assertTrue(WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get().isEmpty())

        SmsCaptureWork.runner = SmsCaptureRunner { SmsCaptureSummary(providerError = true) }
        val failed = TestListenableWorkerBuilder<SmsCaptureWorker>(context).build()
        assertEquals(androidx.work.ListenableWorker.Result.retry(), failed.startWork().get())
        assertTrue(WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get().isEmpty())
        assertEquals(BackoffPolicy.EXPONENTIAL, SmsCaptureWork.createRequest(SmsCaptureRequestSource.DEFAULT, false).workSpec.backoffPolicy)
        assertEquals(30_000L, SmsCaptureWork.createRequest(SmsCaptureRequestSource.DEFAULT, false).workSpec.backoffDelayDuration)
    }

    @Test fun disabledDiagnosticsDoNotTouchInputsClockOrSinkAndDoNotChangeWorkerResult() {
        var runs = 0
        SmsCaptureWork.runner = SmsCaptureRunner { runs++; SmsCaptureSummary() }
        val disabled = SyncDiagnostics.withTestConfiguration(
            false,
            { error("clock should not be read") },
            { error("cpu should not be read") },
            { error("sink should not be called") },
        ) {
            TestListenableWorkerBuilder<SmsCaptureWorker>(context).build().startWork().get()
        }
        val events = mutableListOf<String>()
        val enabled = SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add) {
            TestListenableWorkerBuilder<SmsCaptureWorker>(context).build().startWork().get()
        }

        assertEquals(androidx.work.ListenableWorker.Result.success(), disabled)
        assertEquals(disabled, enabled)
        assertEquals(2, runs)
        assertTrue(events.any { it.contains("event=${SyncTraceEvent.CAPTURE_START.code}") })
    }

    @Test fun ensureObserverRegistersOnceAndRevocationCancelsItsPendingDispatch() {
        val application = context.applicationContext as android.app.Application
        val resolver = context.contentResolver
        val resolverShadow = shadowOf(resolver)
        shadowOf(application).grantPermissions(Manifest.permission.READ_SMS)
        SmsCaptureLifecycle.ensureObserver(context)
        SmsCaptureLifecycle.ensureObserver(context)
        assertEquals(1, resolverShadow.getContentObservers(Telephony.Sms.CONTENT_URI).size)

        resolver.notifyChange(Telephony.Sms.CONTENT_URI, null)
        ShadowLooper.idleMainLooper()

        shadowOf(application).denyPermissions(Manifest.permission.READ_SMS)
        SmsCaptureLifecycle.ensureObserver(context)
        assertEquals(0, resolverShadow.getContentObservers(Telephony.Sms.CONTENT_URI).size)
        // Re-grant before the deadline so an uncancelled callback cannot pass merely because permission stayed denied.
        shadowOf(application).grantPermissions(Manifest.permission.READ_SMS)
        ShadowLooper.idleMainLooper(20, TimeUnit.SECONDS)

        assertTrue(WorkManager.getInstance(context).getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get().isEmpty())
    }

    @Test fun workerRejectsPriorSessionAttributionAndDoesNotMeasureAZeroDelay() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val events = mutableListOf<String>()
        val input = Data.Builder()
            .putLong(SmsCaptureWork.DIAGNOSTIC_TRACE_ID, 77L)
            .putLong(SmsCaptureWork.DIAGNOSTIC_SESSION, 1L)
            .putLong(SmsCaptureWork.DIAGNOSTIC_ENQUEUED_MS, 0L)
            .putLong(SmsCaptureWork.DIAGNOSTIC_SOURCE, SmsCaptureRequestSource.OBSERVER.code)
            .build()
        val previous = SmsCaptureWork.runner
        SmsCaptureWork.runner = SmsCaptureRunner { SmsCaptureSummary() }
        try {
            SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add, sessionToken = 2L) {
                val worker = TestListenableWorkerBuilder<SmsCaptureWorker>(context).setInputData(input).build()
                assertEquals(androidx.work.ListenableWorker.Result.success(), worker.startWork().get())
            }
        } finally {
            SmsCaptureWork.runner = previous
        }
        val start = events.single { it.contains("event=${SyncTraceEvent.CAPTURE_START.code}") }
        assertTrue(start.contains("${SyncTraceField.UNAVAILABLE.code}=1"))
        assertFalse(start.contains("${SyncTraceField.DELAY_MS.code}="))
        assertFalse(start.contains("trace=77"))
    }

    @Test fun workerAcceptsOnlySameSessionNonfutureQueueMetadata() {
        val events = mutableListOf<String>()
        SmsCaptureWork.runner = SmsCaptureRunner { SmsCaptureSummary() }
        val valid = diagnosticInput(trace = 11, session = 7, enqueuedMs = 0)
        val future = diagnosticInput(trace = 12, session = 7, enqueuedMs = 101)
        val oldSession = diagnosticInput(trace = 13, session = 6, enqueuedMs = 0)

        SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add, sessionToken = 7L) {
            listOf(valid, Data.EMPTY, future, oldSession).forEach { input ->
                val worker = TestListenableWorkerBuilder<SmsCaptureWorker>(context).setInputData(input).build()
                assertEquals(androidx.work.ListenableWorker.Result.success(), worker.startWork().get())
            }
        }

        val starts = events.filter { it.contains("event=${SyncTraceEvent.CAPTURE_START.code}") }.map(::fields)
        assertEquals(4, starts.size)
        assertEquals(100L, starts[0][SyncTraceField.DELAY_MS.code])
        assertEquals(SmsCaptureRequestSource.UNKNOWN.code, starts[1][SyncTraceField.SOURCE.code])
        starts.drop(1).forEach { fields ->
            assertEquals(1L, fields[SyncTraceField.UNAVAILABLE.code])
            assertFalse(fields.containsKey(SyncTraceField.DELAY_MS.code))
        }
    }

    private fun diagnosticInput(trace: Long, session: Long, enqueuedMs: Long): Data = Data.Builder()
        .putLong(SmsCaptureWork.DIAGNOSTIC_TRACE_ID, trace)
        .putLong(SmsCaptureWork.DIAGNOSTIC_SESSION, session)
        .putLong(SmsCaptureWork.DIAGNOSTIC_ENQUEUED_MS, enqueuedMs)
        .putLong(SmsCaptureWork.DIAGNOSTIC_SOURCE, SmsCaptureRequestSource.OBSERVER.code)
        .build()

    private fun fields(event: String): Map<Long, Long> = event.split(' ').mapNotNull { token ->
        token.substringBefore('=').toLongOrNull()?.let { key -> token.substringAfter('=', "").toLongOrNull()?.let { key to it } }
    }.toMap()

    private fun awaitUniqueWorkTerminal(manager: WorkManager, expectedCount: Int): List<WorkInfo> {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        var latest = emptyList<WorkInfo>()
        while (System.nanoTime() < deadline) {
            ShadowLooper.runUiThreadTasksIncludingDelayedTasks()
            latest = manager.getWorkInfosForUniqueWork(SmsCaptureWork.NAME).get(1, TimeUnit.SECONDS)
            if (latest.size > expectedCount) fail("unexpected continuation loop: $latest")
            if (latest.size == expectedCount && latest.all { it.state.isFinished }) return latest
            Thread.yield()
        }
        fail("unique work did not reach $expectedCount terminal entries: $latest")
        throw AssertionError("unreachable")
    }
}
