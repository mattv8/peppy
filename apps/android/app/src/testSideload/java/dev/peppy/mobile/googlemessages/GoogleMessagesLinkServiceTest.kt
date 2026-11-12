package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import org.json.JSONObject
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.android.controller.ServiceController
import org.robolectric.annotation.Config
import dev.peppy.mobile.googlemessages.live.LiveTypingCredentials
import dev.peppy.mobile.googlemessages.live.LiveTypingListener
import uniffi.peppy_mobile_bindings.NativeLiveClock
import uniffi.peppy_mobile_bindings.NativeLiveEnableContext
import uniffi.peppy_mobile_bindings.NativeLiveOutbound
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeTypingState
import uniffi.peppy_mobile_bindings.NativeRcsContextExpectation
import uniffi.peppy_mobile_bindings.NativeRcsIngestOutcome
import uniffi.peppy_mobile_bindings.NativeRcsObservationProvenance
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkServiceTest {
    @Test fun `journaled command wake starts media discovery without service restart`() {
        installEngine(StatusHandle(UNLINKED_STATUS))
        val controller = Robolectric.buildService(GoogleMessagesLinkService::class.java).create()
        var sent = 0
        val work = RcsNativeSendWork(
            enabled = { true }, candidates = { listOf(RcsNativeCommand("media-command", "conversation", "peer")) },
            execute = { sent++ }, current = { true }, executor = Executor(Runnable::run),
        )
        GoogleMessagesLinkService::class.java.getDeclaredField("mediaRcsSendWork").apply { isAccessible = true }.set(controller.get(), work)
        try {
            GoogleMessagesLinkService.registerSendWake()
            dev.peppy.mobile.RcsSendWake.notifyCommandsJournaled()
            assertEquals(1, sent)
        } finally { controller.destroy() }
    }
    @Before
    fun setUp() {
        LinkCoreBridge.recorder = { _, _ -> true }
    }

    @After
    fun cleanUp() {
        GoogleMessagesLinkService.stopActiveWorker()
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkRuntimeTestHooks.eventRecorder = null
        LinkCoreBridge.resetForTest()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = null
    }


    @Test fun `restart sweep does not run before ready`() {
        val handle = BlockingRunHandle()
        val executor = QueueExecutor()
        val sweeps = AtomicInteger()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            receiveSweepExecutor = executor,
            receiveSweep = { _, _ -> sweeps.incrementAndGet() },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        executor.runAll()

        assertEquals(0, sweeps.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `native RCS gate defaults on and honors the test override`() {
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = null
        assertTrue(GoogleMessagesLinkService.nativeRcsEnabled())

        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = false
        assertFalse(GoogleMessagesLinkService.nativeRcsEnabled())
    }

    @Test fun `restart sweep fetches once and closes an empty gap without a completion write`() {
        val handle = ReadyGapHandle()
        val core = ReadySweepCore()
        val coordinator = RcsReceiveCoordinator(
            ApplicationProvider.getApplicationContext(),
            core,
            enabled = { true },
        )
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            receiveSweepExecutor = Executor(Runnable::run),
            bootstrapExecutor = Executor(Runnable::run),
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
            receiveSweep = { lease, cancelled -> coordinator.resume(lease.handle, cancelled) },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        assertEquals(0, handle.gapRequests.get())
        assertEquals(0, core.completions.get())

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(1, handle.gapRequests.get())
        assertEquals(0, core.completions.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `restart sweep runs again after reconnect ready`() {
        val handle = ReconnectRunHandle()
        val executor = QueueExecutor()
        val sweeps = AtomicInteger()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            sleeper = {},
            receiveSweepExecutor = executor,
            bootstrapExecutor = Executor(Runnable::run),
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
            receiveSweep = { _, _ -> sweeps.incrementAndGet() },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.firstRunEntered.await(1, TimeUnit.SECONDS))
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        executor.runAll()
        handle.allowFirstRunReturn.countDown()
        assertTrue(handle.secondRunEntered.await(1, TimeUnit.SECONDS))
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        executor.runAll()

        assertEquals(2, sweeps.get())
        handle.allowSecondRunReturn.countDown()
        controller.destroy()
    }

    @Test fun `stale run generation sweep is suppressed after reconnect`() {
        val handle = ReconnectRunHandle()
        val executor = QueueExecutor()
        val sweeps = AtomicInteger()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            sleeper = {},
            receiveSweepExecutor = executor,
            bootstrapExecutor = Executor(Runnable::run),
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
            receiveSweep = { _, _ -> sweeps.incrementAndGet() },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.firstRunEntered.await(1, TimeUnit.SECONDS))
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        handle.allowFirstRunReturn.countDown()
        assertTrue(handle.secondRunEntered.await(1, TimeUnit.SECONDS))
        executor.runAll()
        assertEquals(0, sweeps.get())

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        executor.runAll()
        assertEquals(1, sweeps.get())
        handle.allowSecondRunReturn.countDown()
        controller.destroy()
    }

    @Test fun `overlapping ready wakes remain single flight and coalesce a followup`() {
        val handle = BlockingRunHandle()
        val active = AtomicInteger()
        val maximumActive = AtomicInteger()
        val sweeps = AtomicInteger()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            receiveSweepExecutor = Executor(Runnable::run),
            bootstrapExecutor = Executor(Runnable::run),
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
            receiveSweep = { _, _ ->
                val nowActive = active.incrementAndGet()
                maximumActive.updateAndGet { maxOf(it, nowActive) }
                if (sweeps.incrementAndGet() == 1) GoogleMessagesLinkService.onLinkEvent("conn", "ready")
                active.decrementAndGet()
            },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(2, sweeps.get())
        assertEquals(1, maximumActive.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `ready event has no restart sweep effect while the native gate is off`() {
        val handle = BlockingRunHandle()
        val executor = QueueExecutor()
        val sweeps = AtomicInteger()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = false
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            receiveSweepExecutor = executor,
            receiveSweep = { _, _ -> sweeps.incrementAndGet() },
        )
        installEngine(handle)

        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        executor.runAll()

        assertEquals(0, sweeps.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test
    fun `cancel before run entry closes the handle and native run rejects admission`() {
        val handle = FenceHandle()
        val beforeRun = CountDownLatch(1)
        val interruptObserved = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(beforeRun = {
            beforeRun.countDown()
            var closed = false
            while (!closed) {
                try {
                    handle.closeCalled.await()
                    closed = true
                } catch (_: InterruptedException) {
                    interruptObserved.countDown()
                }
            }
        })
        installEngine(handle)
        val controller = startService()
        assertTrue(beforeRun.await(1, TimeUnit.SECONDS))

        controller.destroy()

        assertTrue(interruptObserved.await(1, TimeUnit.SECONDS))
        assertTrue(handle.closeCalled.await(1, TimeUnit.SECONDS))
        assertTrue(handle.closed)
        assertTrue(handle.runRejected.await(1, TimeUnit.SECONDS))
        assertEquals(1, handle.runCalls.get())
    }

    @Test
    fun `prestart recording cannot block main-thread service destruction or pairing admission`() {
        val handle = BlockingRunHandle()
        val recorderEntered = CountDownLatch(1)
        val releaseRecorder = CountDownLatch(1)
        val mainCallStarted = CountDownLatch(1)
        val mainCallReturned = CountDownLatch(1)
        val returnedBeforeRelease = AtomicBoolean(false)
        val blockRecorder = AtomicBoolean(false)
        LinkCoreBridge.recorder = { _, _ ->
            if (blockRecorder.get()) {
                recorderEntered.countDown()
                releaseRecorder.await()
            }
            true
        }
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        blockRecorder.set(true)
        val firstAttempt = checkNotNull(LinkRuntime.tryBeginPairing())
        val preStart = Thread { LinkRuntime.recordPreStart(controller.get(), firstAttempt) }.also { it.start() }
        assertTrue(recorderEntered.await(1, TimeUnit.SECONDS))
        assertTrue(LinkRuntime.endPairing(firstAttempt))
        val releaseAfterObservation = Thread {
            assertTrue(mainCallStarted.await(1, TimeUnit.SECONDS))
            returnedBeforeRelease.set(mainCallReturned.await(250, TimeUnit.MILLISECONDS))
            releaseRecorder.countDown()
        }.also { it.start() }

        mainCallStarted.countDown()
        controller.destroy()
        val nextAttempt = LinkRuntime.tryBeginPairing()
        mainCallReturned.countDown()

        releaseAfterObservation.join(1_000)
        preStart.join(1_000)
        assertTrue("main-looper destruction and admission waited for lifecycle persistence", returnedBeforeRelease.get())
        assertTrue(nextAttempt != null)
        nextAttempt?.let { LinkRuntime.endPairing(it) }
        handle.allowRunReturn.countDown()
    }

    @Test
    fun `old service destroy cannot close the new service handle`() {
        val oldHandle = BlockingRunHandle()
        val newHandle = BlockingRunHandle()
        installEngine(oldHandle, newHandle)
        val oldController = startService()
        assertTrue(oldHandle.runEntered.await(1, TimeUnit.SECONDS))

        val newController = startService()
        assertTrue(newHandle.runEntered.await(1, TimeUnit.SECONDS))
        oldController.destroy()

        assertTrue(oldHandle.closed)
        assertFalse(newHandle.closed)
        newController.destroy()
        oldHandle.allowRunReturn.countDown()
        newHandle.allowRunReturn.countDown()
    }

    @Test
    fun `pairing wait uses monotonic clock and covers the full native budget`() {
        val handle = StatusHandle(PAIRING_STATUS)
        installEngine(handle)
        var elapsed = 0L
        val finished = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            elapsedRealtime = { elapsed },
            sleeper = { elapsed += 30_000L },
            workerFinished = { finished.countDown() },
        )
        val attempt = LinkRuntime.tryBeginPairing()!!

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertEquals(390_000L, elapsed)
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `conn ready resets retry backoff and run refused terminates without hot loop`() {
        val handle = RetryHandle()
        installEngine(handle)
        val delays = mutableListOf<Long>()
        val finished = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            sleeper = { delay ->
                delays += delay
                if (delays.size == 1) GoogleMessagesLinkService.onLinkEvent("conn", "ready")
            },
            workerFinished = { finished.countDown() },
        )

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertEquals(listOf(30_000L, 30_000L), delays)
        assertEquals(3, handle.runCalls.get())
    }

    @Test
    fun `destroyed service refresh cannot publish after a held status read`() {
        val oldHandle = HeldRefreshHandle()
        val newHandle = BlockingRunHandle()
        installEngine(oldHandle, newHandle)
        val publishes = mutableListOf<GoogleMessagesLinkService>()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = Executor(Runnable::run),
            notificationPublished = { service -> synchronized(publishes) { publishes += service } },
        )
        val oldController = startService()
        val oldService = oldController.get()
        assertTrue(oldHandle.runEntered.await(1, TimeUnit.SECONDS))
        val beforeRefresh = synchronized(publishes) { publishes.count { it === oldService } }

        val refresh = Thread { GoogleMessagesLinkService.onLinkEvent("conn", "ready") }.also { it.start() }
        assertTrue(oldHandle.refreshStatusEntered.await(1, TimeUnit.SECONDS))
        val newController = startService()
        assertTrue(newHandle.runEntered.await(1, TimeUnit.SECONDS))
        oldHandle.allowRefreshStatus.countDown()
        assertTrue(oldHandle.refreshStatusReturned.await(1, TimeUnit.SECONDS))
        refresh.join(1_000)

        assertEquals(beforeRefresh, synchronized(publishes) { publishes.count { it === oldService } })
        oldController.destroy()
        newController.destroy()
        oldHandle.allowRunReturn.countDown()
        newHandle.allowRunReturn.countDown()
    }

    @Test
    fun `event notification refresh is submitted to the serialized executor`() {
        val executor = QueueExecutor()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(notificationExecutor = executor)
        installEngine(StatusHandle(UNLINKED_STATUS))
        val controller = Robolectric.buildService(GoogleMessagesLinkService::class.java).create()

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(1, executor.size)
        controller.destroy()
    }

    @Test fun `diagnostic callbacks never schedule typing readiness or notification work`() {
        val executor = QueueExecutor()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = executor, typingExecutor = executor,
            bootstrapExecutor = executor, eligibilityExecutor = executor,
        )
        installEngine(StatusHandle(UNLINKED_STATUS))
        val controller = Robolectric.buildService(GoogleMessagesLinkService::class.java).create()
        listOf("native_prepare", "conversation_refresh", "conversation_scan", "conversation_rpc").forEach { kind ->
            GoogleMessagesLinkService.onLinkEvent(kind, "fixed_diagnostic")
        }
        assertEquals(0, executor.size)
        controller.destroy()
    }

    @Test
    fun `link events record refreshed capability facts while Run blocks`() {
        val handle = MutableStatusRunHandle()
        val recorded = mutableListOf<LinkStatus>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status }; true }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(notificationExecutor = Executor(Runnable::run))
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertTrue(synchronized(recorded) { READY_STATUS_PARSED in recorded })
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `ready records lifecycle before proactive eligibility enumeration`() {
        val handle = MutableStatusRunHandle()
        val recorded = mutableListOf<LinkStatus>()
        val lifecycleWasRecorded = AtomicBoolean(false)
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status }; true }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = Executor(Runnable::run),
            beforeEligibilityEnumeration = {
                lifecycleWasRecorded.set(synchronized(recorded) { READY_STATUS_PARSED in recorded })
            },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertTrue(lifecycleWasRecorded.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `ready and recovered publish eligibility for linked mapped conversations`() {
        val handle = MutableStatusRunHandle()
        val enumerations = AtomicInteger()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = Executor(Runnable::run),
            beforeEligibilityEnumeration = { enumerations.incrementAndGet() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        GoogleMessagesLinkService.onLinkEvent("conn", "recovered")

        assertEquals(2, enumerations.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `index stale records lifecycle and queues eligibility publication`() {
        val handle = MutableStatusRunHandle()
        val executor = QueueExecutor()
        val recorded = mutableListOf<LinkStatus>()
        val enumerations = AtomicInteger()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status }; true }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            eligibilityExecutor = executor,
            notificationExecutor = Executor(Runnable::run),
            beforeEligibilityEnumeration = { enumerations.incrementAndGet() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("index", "stale")

        assertTrue(synchronized(recorded) { READY_STATUS_PARSED in recorded })
        assertEquals(1, executor.size)
        executor.runAll()
        assertEquals(1, enumerations.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `refused lifecycle prevents index stale eligibility publication`() {
        val handle = MutableStatusRunHandle()
        val executor = QueueExecutor()
        val enumerations = AtomicInteger()
        val refuseLifecycle = AtomicBoolean(false)
        LinkCoreBridge.recorder = { _, _ -> !refuseLifecycle.get() }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            eligibilityExecutor = executor,
            notificationExecutor = Executor(Runnable::run),
            beforeEligibilityEnumeration = { enumerations.incrementAndGet() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        refuseLifecycle.set(true)
        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("index", "stale")

        assertEquals(0, executor.size)
        assertEquals(0, enumerations.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `conversation updates continue to coalesce eligibility publication`() {
        val handle = MutableStatusRunHandle()
        val executor = QueueExecutor()
        val enumerations = AtomicInteger()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            eligibilityExecutor = executor,
            notificationExecutor = Executor(Runnable::run),
            beforeEligibilityEnumeration = { enumerations.incrementAndGet() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("conversation", "updated")
        GoogleMessagesLinkService.onLinkEvent("conversation", "updated")

        assertEquals(1, executor.size)
        executor.runAll()
        assertEquals(1, enumerations.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `conversation updates and recovered streams refresh the index inside the full bootstrap interval`() {
        val handle = RefreshTrackingHandle(listOf("ok", "ok"))
        val executor = QueueExecutor()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            bootstrapExecutor = executor,
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        GoogleMessagesLinkService.onLinkEvent("conversation", "updated")
        executor.runAll()
        GoogleMessagesLinkService.onLinkEvent("conn", "recovered")
        executor.runAll()

        assertEquals(2, handle.refreshes.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `readiness reuses a usable index instead of rescanning it`() {
        val handle = object : MutableStatusRunHandle() {
            val ensures = AtomicInteger()
            val rescans = AtomicInteger()
            override fun ensureConversationIndex(): String = "{\"code\":\"ok\"}".also { ensures.incrementAndGet() }
            override fun refreshConversationIndex(): String = "{\"code\":\"ok\"}".also { rescans.incrementAndGet() }
        }
        val executor = QueueExecutor()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            bootstrapExecutor = executor,
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        executor.runAll()

        assertEquals("ensures=1 rescans=0", "ensures=${handle.ensures.get()} rescans=${handle.rescans.get()}")
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `readiness retries after an incomplete index and recovers owned conversations after refresh`() {
        val events = mutableListOf<String>()
        val handle = RefreshTrackingHandle(listOf("stale", "ok"), events)
        val executor = QueueExecutor()
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = true
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            bootstrapExecutor = executor,
            bootstrapFactory = { _, _, _ -> emptyBootstrap(events) },
            receiveSweepExecutor = Executor(Runnable::run),
            receiveSweep = { _, _ -> events += "sweep-owned" },
        )
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
        assertEquals(1, executor.size)
        executor.runAll()
        GoogleMessagesLinkService.onLinkEvent("conn", "recovered")
        executor.runAll()

        assertEquals(listOf("refresh:stale", "refresh:ok", "snapshot", "sweep-owned"), events)
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test
    fun `Run event overflow records the fixed diagnostic code`() {
        val events = mutableListOf<Pair<String, String>>()
        val finished = CountDownLatch(1)
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> events += kind to code }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            sleeper = { throw InterruptedException() },
            workerFinished = { finished.countDown() },
        )
        installEngine(OverflowHandle())

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertTrue(events.contains("link" to "event_overflow"))
    }

    @Test
    fun `terminal notification is detached and published before destroy`() {
        val executor = QueueExecutor()
        val published = CountDownLatch(1)
        val finished = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = executor,
            notificationPublished = { published.countDown() },
            workerFinished = { finished.countDown() },
        )
        installEngine(StatusHandle(NEEDS_RELINK_STATUS))

        val controller = startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        controller.destroy()
        assertTrue(published.await(1, TimeUnit.SECONDS))
    }

    @Test
    fun `terminal attention fences queued foreground updates`() {
        val executor = QueueExecutor()
        val finished = CountDownLatch(1)
        var publications = 0
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = executor,
            notificationPublished = { publications++ },
            workerFinished = { finished.countDown() },
        )
        installEngine(StatusHandle(NEEDS_RELINK_STATUS))

        val controller = startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        // The first foreground promotion bypasses the queued refresh executor.
        assertEquals(2, publications)
        executor.runAll()
        assertEquals(2, publications)
        controller.destroy()
    }

    @Test
    fun `failed lifecycle fence prevents native link open`() {
        var opens = 0
        val finished = CountDownLatch(1)
        LinkCoreBridge.recorder = { _, _ -> false }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(workerFinished = { finished.countDown() })
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = StatusHandle(UNLINKED_STATUS).also { opens++ }
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertEquals(0, opens)
    }

    @Test
    fun `pairing service observation does not record unlinked`() {
        val recordedStates = mutableListOf<String>()
        val finished = CountDownLatch(1)
        var elapsed = 0L
        LinkCoreBridge.recorder = { _, status -> synchronized(recordedStates) { recordedStates += status.state }; true }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            elapsedRealtime = { elapsed },
            sleeper = { elapsed += it },
            workerFinished = { finished.countDown() },
        )
        installEngine(StatusHandle(UNLINKED_STATUS))
        val pairing = LinkRuntime.tryBeginPairing()!!

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertFalse(synchronized(recordedStates) { "unlinked" in recordedStates })
        assertTrue(LinkRuntime.endPairing(pairing))
    }

    @Test
    fun `unlinked status read before a completed pairing fence is not recorded`() {
        val statusRead = CountDownLatch(1)
        val allowSubmission = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val recordedStates = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recordedStates) { recordedStates += status.state }; true }
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            afterStatusRead = {
                statusRead.countDown()
                allowSubmission.await()
            },
            workerFinished = { finished.countDown() },
        )
        installEngine(StatusHandle(UNLINKED_STATUS))

        startService()

        assertTrue(statusRead.await(1, TimeUnit.SECONDS))
        val pairing = LinkRuntime.tryBeginPairing()!!
        assertTrue(LinkCoreBridge.recordPreStart(androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()))
        assertTrue(LinkRuntime.endPairing(pairing))
        allowSubmission.countDown()
        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertFalse(synchronized(recordedStates) { "unlinked" in recordedStates })
    }

    @Test
    fun `terminal after run is recorded before destroy can suppress notification`() {
        listOf(NEEDS_RELINK_STATUS, SIGNED_OUT_STATUS).forEach { terminalStatus ->
            val recordEntered = CountDownLatch(1)
            val releaseRecord = CountDownLatch(1)
            val terminalRecorded = CountDownLatch(1)
            val notificationExecutor = QueueExecutor()
            var publications = 0
            LinkCoreBridge.recorder = { _, status ->
                if (status.state == LinkStatus.parse(terminalStatus).state) {
                    terminalRecorded.countDown()
                    recordEntered.countDown()
                    releaseRecord.await()
                }
                true
            }
            GoogleMessagesLinkService.testHooks = ServiceTestHooks(
                notificationExecutor = notificationExecutor,
                notificationPublished = { publications++ },
            )
            installEngine(TerminalAfterRunHandle(terminalStatus))
            val controller = startService()
            assertTrue(recordEntered.await(1, TimeUnit.SECONDS))

            controller.destroy()
            releaseRecord.countDown()

            assertTrue(terminalRecorded.await(1, TimeUnit.SECONDS))
            // The initial content-free foreground promotion is synchronous.
            assertEquals(1, publications)
            LinkRuntime.resetForTest()
            LinkCoreBridge.resetForTest()
        }
    }

    @Test
    fun `destroy returns while worker is blocked opening native handle`() {
        val openEntered = CountDownLatch(1)
        val releaseOpen = CountDownLatch(1)
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle {
                openEntered.countDown()
                releaseOpen.await()
                return StatusHandle(UNLINKED_STATUS)
            }
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }
        val controller = startService()
        assertTrue(openEntered.await(1, TimeUnit.SECONDS))

        val startedAt = System.nanoTime()
        controller.destroy()
        val elapsedMs = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - startedAt)

        assertTrue("destroy blocked for $elapsedMs ms", elapsedMs < 250)
        releaseOpen.countDown()
    }

    @Test fun `conn ready starts one typing host and a repeated ready does not start a second`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val controller = startTypingService(handle, typing)
        assertEquals(0, typing.transport.starts.get())

        goLinkedAndRunning(handle)
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(1, typing.transport.starts.get())
        assertTrue(typing.transport.running)
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `unlink stops the typing host and disables the bridge`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        startTypingService(handle, typing)
        goLinkedAndRunning(handle)
        assertTrue(typing.transport.running)

        LinkRuntime.resetLocalState(ApplicationProvider.getApplicationContext())

        assertFalse(typing.transport.running)
        assertTrue(typing.core.disables.get() > 0)
        handle.allowRunReturn.countDown()
    }

    @Test fun `link loss stops the typing host and disables the bridge`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val controller = startTypingService(handle, typing)
        goLinkedAndRunning(handle)
        assertTrue(typing.transport.running)

        handle.status = LINKED_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "temporary_error")

        assertFalse(typing.transport.running)
        assertTrue(typing.core.disables.get() > 0)
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `vault lock or credential loss stops the typing host and disables the bridge`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val controller = startTypingService(handle, typing)
        goLinkedAndRunning(handle)
        assertTrue(typing.transport.running)

        typing.session = null
        GoogleMessagesLinkService.onLinkEvent("conn", "temporary_error")

        assertFalse(typing.transport.running)
        assertTrue(typing.core.disables.get() > 0)
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `service destroy stops the typing host and disables the bridge`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val controller = startTypingService(handle, typing)
        goLinkedAndRunning(handle)
        assertTrue(typing.transport.running)

        controller.destroy()

        assertFalse(typing.transport.running)
        assertTrue(typing.core.disables.get() > 0)
        handle.allowRunReturn.countDown()
    }

    @Test fun `destroyed service cannot start a queued typing update`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val executor = QueuedExecutor()
        installTypingHooks(typing, typingExecutor = executor)
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        goLinkedAndRunning(handle)
        controller.destroy()
        executor.runAll()

        assertEquals(0, typing.transport.starts.get())
        handle.allowRunReturn.countDown()
    }

    @Test fun `typing host never starts while the native RCS gate is off`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        installTypingHooks(typing, nativeRcs = false)
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))

        goLinkedAndRunning(handle)
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(0, typing.transport.starts.get())
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    @Test fun `conn recovered restarts the typing host after a stop caused by link loss`() {
        val handle = MutableStatusRunHandle()
        val typing = TypingFixture()
        val controller = startTypingService(handle, typing)
        goLinkedAndRunning(handle)
        handle.status = LINKED_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "temporary_error")
        assertFalse(typing.transport.running)

        goLinkedAndRunning(handle)
        GoogleMessagesLinkService.onLinkEvent("conn", "recovered")

        assertEquals(2, typing.transport.starts.get())
        assertTrue(typing.transport.running)
        controller.destroy()
        handle.allowRunReturn.countDown()
    }

    private fun startTypingService(handle: MutableStatusRunHandle, typing: TypingFixture): ServiceController<GoogleMessagesLinkService> {
        installTypingHooks(typing)
        installEngine(handle)
        val controller = startService()
        assertTrue(handle.runEntered.await(1, TimeUnit.SECONDS))
        return controller
    }

    /** Typing starts only from a running linked status; the service run is entered in the linked, not running state. */
    private fun goLinkedAndRunning(handle: MutableStatusRunHandle) {
        handle.status = READY_STATUS
        GoogleMessagesLinkService.onLinkEvent("conn", "ready")
    }

    private fun installTypingHooks(
        typing: TypingFixture,
        nativeRcs: Boolean = true,
        typingExecutor: Executor = Executor(Runnable::run),
    ) {
        GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest = nativeRcs
        val direct = Executor(Runnable::run)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = direct,
            eligibilityExecutor = direct,
            bootstrapExecutor = direct,
            bootstrapFactory = { _, _, _ -> emptyBootstrap() },
            receiveSweepExecutor = direct,
            receiveSweep = { _, _ -> },
            typingBridgeFactory = { context ->
                TypingBridge(context = context, host = typing.transport, executor = direct, client = { typing.core })
            },
            typingSession = { typing.session },
            typingExecutor = typingExecutor,
        )
    }

    private class TypingFixture {
        val transport = FakeLiveTypingHost()
        val core = FakeTypingCore()
        @Volatile var session: TypingCredentials? = TypingCredentials("https://example.test", "bearer", "gateway", 1)
    }

    private class FakeLiveTypingHost : TypingTransport {
        val starts = AtomicInteger()
        @Volatile var running = false
        override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) {
            starts.incrementAndGet()
            running = true
        }
        override fun stop() { running = false }
        override fun send(frameJson: String) = true
    }

    private class FakeTypingCore : TypingCore {
        val disables = AtomicInteger()
        override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong = 1UL
        override fun setSharing(enabled: Boolean) = Unit
        override fun disable() { disables.incrementAndGet() }
        override fun receiveContext(googleConversationId: String): TypingReceiveContext? = null
        override fun ownedGoogleConversationId(conversationId: String): String? = null
        override fun seal(
            conversationId: String,
            gatewayDeviceId: String,
            participant: String?,
            state: NativeTypingState,
            clock: NativeLiveClock,
        ): NativeLiveOutbound? = null
        override fun open(frameJson: String, clock: NativeLiveClock): NativeLiveOutcome = NativeLiveOutcome(1UL, null, null, null)
    }

    private class QueuedExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks += command }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }

    private fun startService() = Robolectric.buildService(GoogleMessagesLinkService::class.java)
        .create()
        .startCommand(0, 1)

    private fun installEngine(vararg handles: LinkHandle) {
        val next = ArrayDeque(handles.toList())
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = next.removeFirst()
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }
    }

    private object EmptyStore : LinkStateStorage {
        override fun read(): ByteArray = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }

    private open class StatusHandle(private val currentStatus: String) : LinkHandle {
        @Volatile var closed = false
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        open override fun close() { closed = true }
        override fun status(): String = currentStatus
        override fun refreshConversationIndex(): String = "{\"code\":\"ok\"}"
    }

    private class FenceHandle : StatusHandle(LINKED_STATUS) {
        val closeCalled = CountDownLatch(1)
        val runRejected = CountDownLatch(1)
        val runCalls = AtomicInteger()
        override fun close() {
            super.close()
            closeCalled.countDown()
        }
        override fun run() {
            runCalls.incrementAndGet()
            if (closed) {
                runRejected.countDown()
                throw Exception("link_closed")
            }
        }
    }

    private class BlockingRunHandle : StatusHandle(LINKED_STATUS) {
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        override fun run() {
            runEntered.countDown()
            try {
                allowRunReturn.await()
            } catch (_: InterruptedException) {
                // Cancellation closes the native handle and interrupts the Java wrapper wait.
            }
            if (closed) throw Exception("link_closed")
        }
    }

    private class ReadyGapHandle : StatusHandle(LINKED_STATUS) {
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        val gapRequests = AtomicInteger()

        override fun run() {
            runEntered.countDown()
            allowRunReturn.await()
        }

        override fun fetchRcsGap(googleConversationId: String, pageToken: String): String {
            gapRequests.incrementAndGet()
            return """{"code":"ok","records":[],"signals":[],"next_cursor":"next","has_more":false}"""
        }
    }

    private class ReadySweepCore : RcsReceiveCore {
        val completions = AtomicInteger()
        private val context = NativeRcsReceiveContext(
            "core-conversation",
            "google-conversation",
            7u,
            11u,
            NativeRcsReceiveOwner.LINK,
            1_000L,
            null,
            false,
            NativeRcsContextExpectation(7u, 13u, 17u, 19u),
        )

        override fun context(googleId: String) = context
        override fun contexts() = listOf(context)
        override fun ingest(
            context: NativeRcsReceiveContext,
            record: String,
            provenance: NativeRcsObservationProvenance,
        ) = NativeRcsIngestOutcome("inserted", null)
        override fun recordProgress(context: NativeRcsReceiveContext, targetMs: Long, cursor: String) = "recorded"
        override fun complete(context: NativeRcsReceiveContext, targetMs: Long): String {
            completions.incrementAndGet()
            return "recorded"
        }
        override fun clearHint(context: NativeRcsReceiveContext) = "recorded"
        override fun invalidate(context: NativeRcsReceiveContext, googleId: String, eventAtMs: Long?) = false
    }

    private class ReconnectRunHandle : StatusHandle(LINKED_STATUS) {
        val firstRunEntered = CountDownLatch(1)
        val secondRunEntered = CountDownLatch(1)
        val allowFirstRunReturn = CountDownLatch(1)
        val allowSecondRunReturn = CountDownLatch(1)
        private val runCalls = AtomicInteger()

        override fun run() {
            when (runCalls.incrementAndGet()) {
                1 -> {
                    firstRunEntered.countDown()
                    allowFirstRunReturn.await()
                    throw Exception("temporary")
                }
                2 -> {
                    secondRunEntered.countDown()
                    allowSecondRunReturn.await()
                    throw Exception("run_refused")
                }
            }
        }
    }

    private class RetryHandle : StatusHandle(LINKED_STATUS) {
        val runCalls = AtomicInteger()
        override fun run() {
            when (runCalls.incrementAndGet()) {
                1, 2 -> throw Exception("temporary")
                else -> throw Exception("run_refused")
            }
        }
    }

    private class TerminalAfterRunHandle(private val terminalStatus: String) : StatusHandle(LINKED_STATUS) {
        private val statusCalls = AtomicInteger()
        override fun status(): String = if (statusCalls.incrementAndGet() <= 2) LINKED_STATUS else terminalStatus
        override fun run() = Unit
    }

    private open class MutableStatusRunHandle : StatusHandle(LINKED_STATUS) {
        @Volatile var status = LINKED_STATUS
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        override fun status(): String = status
        override fun run() {
            runEntered.countDown()
            allowRunReturn.await()
        }
    }

    private class RefreshTrackingHandle(
        private val refreshCodes: List<String>,
        private val events: MutableList<String>? = null,
    ) : MutableStatusRunHandle() {
        val refreshes = AtomicInteger()

        override fun refreshConversationIndex(): String {
            val code = refreshCodes[refreshes.getAndIncrement()]
            events?.add("refresh:$code")
            return "{\"code\":\"$code\"}"
        }
    }

    private class OverflowHandle : StatusHandle(LINKED_STATUS) {
        override fun run(): Unit = throw Exception("event_overflow")
    }

    private class HeldRefreshHandle : StatusHandle(LINKED_STATUS) {
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        val refreshStatusEntered = CountDownLatch(1)
        val allowRefreshStatus = CountDownLatch(1)
        val refreshStatusReturned = CountDownLatch(1)
        private val statusCalls = AtomicInteger()
        override fun status(): String {
            if (statusCalls.incrementAndGet() >= 3) {
                refreshStatusEntered.countDown()
                allowRefreshStatus.await()
                refreshStatusReturned.countDown()
            }
            return LINKED_STATUS
        }
        override fun run() {
            runEntered.countDown()
            allowRunReturn.await()
        }
    }

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        val size: Int get() = tasks.size
        override fun execute(command: Runnable) { tasks += command }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }

    private fun emptyBootstrap(events: MutableList<String>? = null) = RcsLinkBootstrap(
        core = object : RcsBootstrapCorePort {
            override fun ownedGoogleIds() = setOf("owned")
            override fun resolve(candidates: List<Pair<String, String>>) = emptyList<uniffi.peppy_mobile_bindings.NativeRcsBootstrapResolution>()
            override fun tuple(snapshot: JSONObject) = RcsNativeContextTuple(1u, 1u, 1u)
            override fun revision(conversationId: String) = 1uL
            override fun lifecycleGeneration() = 1uL
            override fun verify(observation: String, nowMs: Long) = uniffi.peppy_mobile_bindings.NativeRcsMappingVerdict(
                uniffi.peppy_mobile_bindings.NativeRcsMappingVerification.MAPPED,
                null,
            )
            override fun claim(json: String) = uniffi.peppy_mobile_bindings.NativeRcsLinkOwnershipClaimOutcome.CLAIMED
        },
        link = object : RcsBootstrapLinkPort {
            override fun candidateSnapshot(): String {
                events?.add("snapshot")
                return "{\"code\":\"conversation_candidate_snapshot\",\"complete\":true,\"conversations\":[]}"
            }
            override fun observe(request: String) = "{\"code\":\"ok\",\"observation\":\"proof\"}"
        },
        routeReader = { error("no candidates") },
    )

    private companion object {
        const val LINKED_STATUS = """{"seq":1,"state":"linked","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val PAIRING_STATUS = """{"seq":1,"state":"pairing","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val UNLINKED_STATUS = """{"seq":1,"state":"unlinked","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val NEEDS_RELINK_STATUS = """{"seq":1,"state":"needs_relink","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val SIGNED_OUT_STATUS = """{"seq":1,"state":"signed_out","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val READY_STATUS = """{"seq":2,"state":"linked","running":true,"healthy":true,"phone_responding":true,"last_ok_ms":1,"stop_code":""}"""
        val READY_STATUS_PARSED = LinkStatus.parse(READY_STATUS)
    }
}
