package dev.peppy.mobile.googlemessages

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeLiveAction
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeLiveSessionUpdate
import uniffi.peppy_mobile_bindings.NativeTypingState

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkRuntimeTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Before
    fun setUp() {
        LinkCoreBridge.recorder = { _, _ -> true }
    }

    @After
    fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkRuntimeTestHooks.eventRecorder = null
        LinkRuntimeTestHooks.rcsReceiveSink = null
        LinkRuntimeTestHooks.typingReceiveSink = null
        LinkRuntimeTestHooks.callbackExecutor = null
        LinkRuntimeTestHooks.afterClosingCheck = null
        LinkCoreBridge.resetForTest()
    }

    @Test
    fun `handle opens once`() {
        val handle = FakeHandle()
        var opens = 0
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle.also { opens++ }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        val first = LinkRuntime.handle(context) as LinkOpenResult.Open
        val second = LinkRuntime.handle(context) as LinkOpenResult.Open

        assertEquals(1, opens)
        assertSame(first.handle, second.handle)
    }

    @Test
    fun `unavailable engine does not read state`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = false
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("not called")
        }

        assertTrue(LinkRuntime.handle(context) is LinkOpenResult.Unavailable)
    }

    @Test
    fun `typing bridge forwards one admitted intent with the contracted validity field`() {
        val handle = FakeHandle().apply { typingResult = "ok" }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        val result = LinkRuntime.setTyping(context, typingIntent())

        assertEquals("ok", result)
        assertEquals(1, handle.typingRequests.size)
        val request = handle.typingRequests.single()
        assertEquals("{\"conversation_id\":\"conversation\",\"state\":\"started\",\"remaining_validity_ms\":25,\"link_generation\":${request.linkGeneration}}", request.toJson())
        assertTrue(request.linkGeneration > 0)
    }

    @Test
    fun `typing bridge preserves native result and maps invocation failure`() {
        val handle = FakeHandle().apply { typingResult = "typing_refused" }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        assertEquals("typing_refused", LinkRuntime.setTyping(context, typingIntent()))
        handle.typingFailure = IllegalStateException()

        assertEquals("typing_failed", LinkRuntime.setTyping(context, typingIntent()))
    }

    @Test
    fun `typing bridge sends the Go owner generation separately from its Kotlin lease`() {
        val handle = FakeHandle().apply { ownerGeneration = 73 }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))

        assertEquals("ok", LinkRuntime.setTyping(lease, typingIntent()))
        assertEquals(73, handle.typingRequests.single().linkGeneration)
    }

    @Test
    fun `typing bridge uses a new Go owner generation after reconnect on the same Kotlin lease`() {
        val handle = FakeHandle().apply { ownerGeneration = 73 }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))

        assertEquals("ok", LinkRuntime.setTyping(lease, typingIntent()))
        handle.ownerGeneration = 74
        assertEquals("ok", LinkRuntime.setTyping(lease, typingIntent()))

        assertEquals(listOf(73L, 74L), handle.typingRequests.map(LinkTypingRequest::linkGeneration))
    }

    @Test
    fun `typing bridge drops a retired Go owner generation without calling Go`() {
        val handle = FakeHandle().apply { ownerGeneration = 0 }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        assertEquals("typing_stale", LinkRuntime.setTyping(context, typingIntent()))
        assertTrue(handle.typingRequests.isEmpty())
    }

    @Test
    fun `typing bridge is unavailable without the gmlink flavor`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = false
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("not called")
        }

        assertEquals("typing_unavailable", LinkRuntime.setTyping(context, typingIntent()))
    }

    @Test
    fun `typing bridge rejects a retired handle lease`() {
        val handle = FakeHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))
        val detached = requireNotNull(LinkRuntime.detachHandleForServiceStop(lease))

        try {
            assertEquals("typing_stale", LinkRuntime.setTyping(lease, typingIntent()))
            assertTrue(handle.typingRequests.isEmpty())
        } finally {
            LinkRuntime.closeDetachedHandle(detached)
        }
    }

    @Test
    fun `typing bridge drops a retired live incarnation before Go`() {
        val handle = FakeHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        assertEquals("typing_stale", LinkRuntime.setTyping(context, typingIntent(current = false)))
        assertTrue(handle.typingRequests.isEmpty())
    }

    @Test
    fun `typing bridge recalculates validity after native lock wait`() {
        val clock = FakeClock(100UL)
        val handle = BlockingTypingHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))
        val firstDone = CountDownLatch(1)
        val secondStarted = CountDownLatch(1)
        val secondDone = CountDownLatch(1)
        var firstResult: String? = null
        var secondResult: String? = null

        try {
            Thread {
                firstResult = LinkRuntime.setTyping(lease, typingIntent(deadlineMono = 500UL, clock = clock))
                firstDone.countDown()
            }.start()
            assertTrue(handle.typingEntered.await(1, TimeUnit.SECONDS))
            Thread {
                secondStarted.countDown()
                secondResult = LinkRuntime.setTyping(lease, typingIntent(deadlineMono = 500UL, clock = clock))
                secondDone.countDown()
            }.start()
            assertTrue(secondStarted.await(1, TimeUnit.SECONDS))
            assertFalse(secondDone.await(50, TimeUnit.MILLISECONDS))
            clock.now = 500UL
        } finally {
            handle.allowTyping.countDown()
        }

        assertTrue(firstDone.await(1, TimeUnit.SECONDS))
        assertTrue(secondDone.await(1, TimeUnit.SECONDS))
        assertEquals("ok", firstResult)
        assertEquals("typing_expired", secondResult)
        assertEquals(1, handle.typingRequests.size)
    }

    @Test
    fun `adapter retires authority while runtime waits on the native lock`() {
        val handle = BlockingTypingHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))
        val firstDone = CountDownLatch(1)
        val adapterEffectStarted = CountDownLatch(1)
        val adapterDone = CountDownLatch(1)
        val retirementDone = CountDownLatch(1)
        val executor = QueueExecutor()
        val adapter = LiveTypingAdapter(FakeClock(100UL), executor) { intent ->
            adapterEffectStarted.countDown()
            LinkRuntime.setTyping(context, intent)
            adapterDone.countDown()
        }
        adapter.activate(7UL)
        adapter.consume(NativeLiveOutcome(7UL, NativeLiveSessionUpdate("peer", null, 8UL, 1UL), null, null))
        adapter.consume(
            NativeLiveOutcome(
                7UL,
                null,
                NativeLiveAction.GatewayTypingIntent(
                    "peer", 8UL, 1UL, "conversation", null, NativeTypingState.STARTED, 200UL,
                ),
                null,
            ),
        )

        try {
            Thread {
                LinkRuntime.setTyping(lease, typingIntent(deadlineMono = 200UL))
                firstDone.countDown()
            }.start()
            assertTrue(handle.typingEntered.await(1, TimeUnit.SECONDS))
            Thread { executor.runAll() }.start()
            assertTrue(adapterEffectStarted.await(1, TimeUnit.SECONDS))
            Thread {
                adapter.consume(NativeLiveOutcome(7UL, NativeLiveSessionUpdate("peer", null, 9UL, 2UL), null, null))
                retirementDone.countDown()
            }.start()

            assertTrue(retirementDone.await(1, TimeUnit.SECONDS))
        } finally {
            handle.allowTyping.countDown()
        }

        assertTrue(firstDone.await(1, TimeUnit.SECONDS))
        assertTrue(adapterDone.await(1, TimeUnit.SECONDS))
        assertEquals(1, handle.typingRequests.size)
    }

    @Test
    fun `typing bridge drops expiry before Go`() {
        val handle = FakeHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        assertEquals("typing_expired", LinkRuntime.setTyping(context, typingIntent(deadlineMono = 100UL, clock = FakeClock(100UL))))
        assertTrue(handle.typingRequests.isEmpty())
    }

    @Test
    fun `unreadable state remains until the owner resets it`() {
        val store = InMemoryStore(unreadable = true)
        LinkRuntimeTestHooks.engine = availableEngine(FakeHandle())
        LinkRuntimeTestHooks.storeFactory = { store }

        val result = LinkRuntime.handle(context)

        assertTrue(result is LinkOpenResult.Unreadable)
        assertFalse(store.wiped)
    }

    @Test
    fun `gomobile open exception becomes filtered unreadable state`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = throw Exception("not safe to show")
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        val result = LinkRuntime.handle(context)

        assertEquals(LinkOpenResult.Unreadable("unknown"), result)
    }

    @Test
    fun `reset stops closes wipes and records a fixed event`() {
        val handle = FakeHandle()
        val events = mutableListOf<String>()
        val store = InMemoryStore()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { store }
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> events += "$kind/$code" }
        LinkRuntime.handle(context)

        LinkRuntime.resetLocalState(context)

        assertTrue(handle.closed)
        assertTrue(store.wiped)
        assertEquals(listOf("link/reset_local"), events)
        assertFalse(LinkRuntime.pairingInProgress.get())
    }


    @Test
    fun `panel observation keeps the token captured before its status read`() {
        val beforeRecord = CountDownLatch(1)
        val releaseRecord = CountDownLatch(1)
        val recordDone = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        val handle = FakeHandle()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val observation = requireNotNull(LinkRuntime.capturePanelObservation(handle))
        val status = LinkStatus.parse(handle.status())
        LinkRuntimeTestHooks.beforePanelObservationRecord = {
            beforeRecord.countDown()
            releaseRecord.await()
        }
        Thread {
            LinkRuntime.recordPanelObservation(context, observation, status)
            recordDone.countDown()
        }.start()
        assertTrue(beforeRecord.await(1, TimeUnit.SECONDS))

        LinkCoreBridge.invalidateObservations()
        assertTrue(LinkCoreBridge.recordExplicitUnlinkCompleted(context))
        releaseRecord.countDown()

        assertTrue(recordDone.await(1, TimeUnit.SECONDS))
        assertEquals(listOf("unlinked"), synchronized(recorded) { recorded.toList() })
    }

    @Test
    fun `reset retires pairing authority before queued prestart publication`() {
        val firstOwnershipCheckPassed = CountDownLatch(1)
        val releasePreStart = CountDownLatch(1)
        val preStartDone = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        var preStartResult = true
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        LinkRuntimeTestHooks.engine = availableEngine(FakeHandle())
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val attempt = requireNotNull(LinkRuntime.tryBeginPairing())
        LinkRuntimeTestHooks.beforePreStartPublication = {
            firstOwnershipCheckPassed.countDown()
            releasePreStart.await()
        }
        Thread {
            preStartResult = LinkRuntime.recordPreStart(context, attempt)
            preStartDone.countDown()
        }.start()
        assertTrue(firstOwnershipCheckPassed.await(1, TimeUnit.SECONDS))

        LinkRuntime.resetLocalState(context)
        releasePreStart.countDown()

        assertTrue(preStartDone.await(1, TimeUnit.SECONDS))
        assertFalse(preStartResult)
        assertEquals(listOf("unlinked"), synchronized(recorded) { recorded.toList() })
    }

    @Test
    fun `reset rejects an observation captured before destructive retirement`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> recorded += status.state; true }
        val handle = FakeHandle()
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val staleToken = LinkCoreBridge.observationToken()

        LinkRuntime.resetLocalState(context)
        assertTrue(LinkCoreBridge.recordObservation(staleToken, LinkRuntime.handleLease(context)!!, context, LinkStatus(0, "linked", true, true, true, 1, "")))

        assertEquals(listOf("unlinked"), recorded)
    }

    @Test
    fun `wipe failure closes captured handle and next reload keeps one open handle`() {
        val first = CountingHandle()
        val second = CountingHandle()
        val store = object : LinkStateStorage {
            override fun read() = ByteArray(0)
            override fun write(state: ByteArray) = Unit
            override fun wipe(): Unit = throw IllegalStateException("wipe_failed")
        }
        var opens = 0
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = if (opens++ == 0) first else second
        }
        LinkRuntimeTestHooks.storeFactory = { store }
        LinkRuntime.handle(context)

        val failure = runCatching { LinkRuntime.resetLocalState(context) }.exceptionOrNull()
        LinkRuntime.handle(context)

        assertEquals("wipe_failed", failure?.message)
        assertEquals(1, first.closeCalls)
        assertEquals(2, opens)
        assertEquals(1, maxOf(first.openHandles, second.openHandles))
    }

    @Test
    fun `failed reset lifecycle record reports persistence failure`() {
        val handle = FakeHandle()
        val store = InMemoryStore()
        LinkCoreBridge.recorder = { _, _ -> false }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { store }
        LinkRuntime.handle(context)

        val failure = runCatching { LinkRuntime.resetLocalState(context) }.exceptionOrNull()

        assertEquals("persist_failed", failure?.message)
        assertTrue(handle.closed)
        assertTrue(store.wiped)
    }

    @Test
    fun `opener cannot overtake a detached handle before native close`() {
        val first = FakeHandle()
        val second = FakeHandle()
        val reachedClosingCheck = CountDownLatch(1)
        val releaseOpener = CountDownLatch(1)
        val openerDone = CountDownLatch(1)
        var opens = 0
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle =
                if (opens++ == 0) first else second
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val lease = LinkRuntime.handleLease(context)!!
        LinkRuntimeTestHooks.afterClosingCheck = {
            reachedClosingCheck.countDown()
            releaseOpener.await()
        }

        Thread { LinkRuntime.handle(context); openerDone.countDown() }.start()
        assertTrue(reachedClosingCheck.await(1, TimeUnit.SECONDS))
        val detached = LinkRuntime.detachHandleForServiceStop(lease)!!
        releaseOpener.countDown()

        Thread.sleep(50)
        assertEquals(1, opens)
        assertFalse(first.closed)
        LinkRuntime.closeDetachedHandle(detached)
        assertTrue(openerDone.await(1, TimeUnit.SECONDS))
        assertEquals(2, opens)
    }

    @Test
    fun `startup creates link notification channel`() {
        GoogleMessagesLink.onAppStart(context)

        val manager = context.getSystemService(NotificationManager::class.java)
        assertEquals(NotificationManager.IMPORTANCE_LOW, manager.getNotificationChannel(GoogleMessagesLinkService.CHANNEL_ID).importance)
    }

    @Test
    fun `new pairing fences and clears a previous emoji`() {
        lateinit var host: LinkHost
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = FakeHandle().also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val first = CountDownLatch(1)
        LinkRuntimeTestHooks.eventRecorder = { _, _, _ -> first.countDown() }

        val firstAttempt = LinkRuntime.tryBeginPairing()!!
        host.showEmojiOwned("first", 0, 0)
        host.eventOwned("link", "pair_ok", 0, 0, 0)
        assertTrue(first.await(1, TimeUnit.SECONDS))
        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(firstAttempt))

        val secondAttempt = LinkRuntime.tryBeginPairing()!!

        assertEquals(null, LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(secondAttempt))
        assertEquals(null, LinkRuntime.emoji.value)
        assertFalse(LinkRuntime.pairingInProgress.get())
    }

    @Test
    fun `second owner cannot clear an active pairing`() {
        val attempt = LinkRuntime.tryBeginPairing()!!
        LinkRuntime.emoji.value = "first"

        assertEquals(null, LinkRuntime.tryBeginPairing())

        assertEquals("first", LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `queued old emoji and late finally cannot affect a new attempt`() {
        lateinit var host: LinkHost
        val queued = QueueExecutor()
        LinkRuntimeTestHooks.callbackExecutor = queued
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = FakeHandle().also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntime.handle(context)
        val oldAttempt = LinkRuntime.tryBeginPairing()!!
        host.showEmojiOwned("old", 0, 0)
        assertTrue(LinkRuntime.endPairing(oldAttempt))
        val newAttempt = LinkRuntime.tryBeginPairing()!!

        assertFalse(LinkRuntime.endPairing(oldAttempt))
        queued.runAll()

        assertEquals(null, LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(newAttempt))
    }

    @Test
    fun `terminal callback never releases pairing ownership`() {
        lateinit var host: LinkHost
        val callbackRan = CountDownLatch(1)
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = FakeHandle().also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntimeTestHooks.eventRecorder = { _, _, _ -> callbackRan.countDown() }
        LinkRuntime.handle(context)
        val attempt = LinkRuntime.tryBeginPairing()!!

        host.eventOwned("link", "pair_ok", 0, 0, 0)
        assertTrue(callbackRan.await(1, TimeUnit.SECONDS))

        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `reset final unlinked publication follows an admitted pairing prestart`() {
        val preStartEntered = CountDownLatch(1)
        val allowPreStart = CountDownLatch(1)
        val resetFinished = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        var preStartResult = true
        LinkCoreBridge.recorder = { _, status ->
            if (status.state == "unknown") {
                preStartEntered.countDown()
                allowPreStart.await()
            }
            synchronized(recorded) { recorded += status.state }
            true
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val attempt = requireNotNull(LinkRuntime.tryBeginPairing())
        val preStart = Thread {
            preStartResult = LinkRuntime.recordPreStart(context, attempt)
        }.also { it.start() }
        assertTrue(preStartEntered.await(1, TimeUnit.SECONDS))
        val reset = Thread {
            LinkRuntime.resetLocalState(context)
            resetFinished.countDown()
        }.also { it.start() }

        assertFalse(resetFinished.await(100, TimeUnit.MILLISECONDS))
        allowPreStart.countDown()
        preStart.join(1_000)
        reset.join(1_000)

        assertFalse(preStart.isAlive)
        assertFalse(reset.isAlive)
        assertFalse(preStartResult)
        assertEquals(listOf("unknown", "unlinked"), synchronized(recorded) { recorded.toList() })
    }

    @Test
    fun `reset after pairing lease closes the admitted handle without opening a replacement`() {
        val handle = object : FakeHandle() {
            var pairCalls = 0
            override fun pair(cookiesJson: ByteArray) {
                check(!closed) { "pair_closed" }
                pairCalls++
            }
        }
        var opens = 0
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle.also { opens++ }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val attempt = requireNotNull(LinkRuntime.tryBeginPairing())
        assertTrue(LinkRuntime.recordPreStart(context, attempt))
        val lease = requireNotNull(LinkRuntime.acquirePairingLease(context, attempt))

        LinkRuntime.resetLocalState(context)
        val failure = runCatching { lease.handle.pair(ByteArray(0)) }.exceptionOrNull()

        assertEquals("pair_closed", failure?.message)
        assertEquals(1, opens)
        assertEquals(0, handle.pairCalls)
    }

    @Test
    fun `stale pairing completion cannot end a newer attempt`() {
        val first = LinkRuntime.tryBeginPairing()!!
        assertTrue(LinkRuntime.endPairing(first))
        val second = LinkRuntime.tryBeginPairing()!!

        assertFalse(LinkRuntime.endPairing(first))

        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(second))
    }

    @Test
    fun `receiver rejects callbacks without owned identity`() {
        lateinit var host: LinkHost
        val queued = QueueExecutor()
        val events = mutableListOf<String>()
        LinkRuntimeTestHooks.callbackExecutor = queued
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = FakeHandle().also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> events += "$kind/$code" }
        LinkRuntime.handle(context)
        val attempt = LinkRuntime.tryBeginPairing()!!

        host.event("link", "pair_ok", 0)
        host.showEmoji("unowned")
        queued.runAll()

        assertTrue(events.isEmpty())
        assertEquals(null, LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `receiver drops callbacks from stale Go ownership`() {
        lateinit var host: LinkHost
        val queued = QueueExecutor()
        var currentGeneration = 3L
        val handle = FakeHandle(accepts = { generation, revision -> generation == currentGeneration && revision == 4L })
        val events = mutableListOf<String>()
        LinkRuntimeTestHooks.callbackExecutor = queued
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = handle.also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> events += "$kind/$code" }
        LinkRuntime.handle(context)
        val attempt = LinkRuntime.tryBeginPairing()!!

        host.eventOwned("link", "pair_ok", 0, 3, 4)
        host.showEmojiOwned("old", 3, 4)
        currentGeneration = 5
        queued.runAll()

        assertTrue(events.isEmpty())
        assertEquals(null, LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `receive sink accepts current owner and drops stale owner`() {
        lateinit var host: LinkHost
        val queued = QueueExecutor()
        val currentGeneration = 3L
        var currentStreamEpoch = 8L
        val records = mutableListOf<String>()
        val handle = FakeHandle(
            accepts = { generation, revision -> generation == currentGeneration && revision == 4L },
            acceptsStream = { generation, revision, streamEpoch ->
                generation == currentGeneration && revision == 4L && streamEpoch == currentStreamEpoch
            },
        )
        LinkRuntimeTestHooks.callbackExecutor = queued
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = handle.also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntimeTestHooks.rcsReceiveSink = RcsReceiveSink { records += it }
        LinkRuntime.handle(context)

        host.rcsEventOwned("accepted", 3, 4, 8)
        queued.runAll()
        host.rcsEventOwned("stale", 3, 4, 8)
        currentStreamEpoch = 9
        queued.runAll()

        assertEquals(listOf("accepted"), records)
    }

    @Test
    fun `typing sink accepts current owner and drops stale owner`() {
        lateinit var host: LinkHost
        val queued = QueueExecutor()
        val currentGeneration = 3L
        var currentStreamEpoch = 8L
        val records = mutableListOf<String>()
        val handle = FakeHandle(
            accepts = { generation, revision -> generation == currentGeneration && revision == 4L },
            acceptsStream = { generation, revision, streamEpoch ->
                generation == currentGeneration && revision == 4L && streamEpoch == currentStreamEpoch
            },
        )
        LinkRuntimeTestHooks.callbackExecutor = queued
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = handle.also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkRuntimeTestHooks.typingReceiveSink = TypingReceiveSink { records += it }
        LinkRuntime.handle(context)

        host.typingEventOwned("accepted", 3, 4, 8)
        queued.runAll()
        host.typingEventOwned("stale", 3, 4, 8)
        currentStreamEpoch = 9
        queued.runAll()

        assertEquals(listOf("accepted"), records)
    }

    @Test
    fun `close during emoji callback finishes`() {
        lateinit var host: LinkHost
        val handle = BlockingCloseHandle()
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, callback: LinkHost): LinkHandle = handle.also { host = callback }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        val lease = requireNotNull(LinkRuntime.handleLease(context))
        val closeFinished = CountDownLatch(1)
        Thread {
            LinkRuntime.closeHandleForServiceStop(lease)
            closeFinished.countDown()
        }.start()
        assertTrue(handle.closeEntered.await(1, TimeUnit.SECONDS))

        val emojiReturned = CountDownLatch(1)
        Thread {
            host.showEmoji("check")
            emojiReturned.countDown()
        }.start()

        assertTrue(emojiReturned.await(1, TimeUnit.SECONDS))
        handle.allowClose.countDown()
        assertTrue(closeFinished.await(1, TimeUnit.SECONDS))
    }

    @Test
    fun `runtime event sink records through the production diagnostic path`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.configureRuntime(enabled = true) { records += it }

        LinkRuntimeEventSink.record("link", "reset_local", 0)

        assertTrue(records.single().contains("event=27"))
        assertTrue(records.single().contains("29=17"))
        SyncDiagnostics.configureRuntime(enabled = false) {}
    }

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks += command }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }

    private fun availableEngine(handle: LinkHandle) = object : LinkEngine {
        override val available = true
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
    }

    private fun typingIntent(
        deadlineMono: ULong = 125UL,
        clock: MonotonicClock = FakeClock(100UL),
        current: Boolean = true,
    ) = LiveTypingEffectIntent(
        "conversation",
        NativeTypingState.STARTED,
        deadlineMono,
        clock,
        LiveTypingCurrency { current },
    )

    private class InMemoryStore(private val unreadable: Boolean = false) : LinkStateStorage {
        var wiped = false

        override fun read(): ByteArray {
            if (unreadable) throw IllegalStateException("link_state_unreadable")
            return ByteArray(0)
        }

        override fun write(state: ByteArray) = Unit
        override fun wipe() { wiped = true }
    }

    private open class FakeHandle(
        private val accepts: (Long, Long) -> Boolean = { _, _ -> true },
        private val acceptsStream: (Long, Long, Long) -> Boolean = { generation, revision, _ -> accepts(generation, revision) },
    ) : LinkHandle {
        var closed = false
        var typingFailure: Throwable? = null
        var typingResult = "ok"
        var ownerGeneration = 1L
        val typingRequests = mutableListOf<LinkTypingRequest>()
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() { closed = true }
        override fun status(): String = """{\"state\":\"unlinked\"}"""
        override fun ownerGeneration(): Long = ownerGeneration
        open override fun setTyping(request: LinkTypingRequest): String {
            typingRequests += request
            typingFailure?.let { throw it }
            return typingResult
        }
        override fun acceptsOutput(generation: Long, revision: Long): Boolean = accepts(generation, revision)
        override fun acceptsStreamOutput(generation: Long, revision: Long, streamEpoch: Long): Boolean =
            acceptsStream(generation, revision, streamEpoch)
    }

    private class BlockingCloseHandle : FakeHandle() {
        val closeEntered = CountDownLatch(1)
        val allowClose = CountDownLatch(1)

        override fun close() {
            closeEntered.countDown()
            allowClose.await()
            super.close()
        }
    }

    private class BlockingTypingHandle : FakeHandle() {
        val typingEntered = CountDownLatch(1)
        val allowTyping = CountDownLatch(1)

        override fun setTyping(request: LinkTypingRequest): String {
            typingEntered.countDown()
            allowTyping.await()
            return super.setTyping(request)
        }
    }

    private class FakeClock(@Volatile var now: ULong) : MonotonicClock {
        override fun nowMs(): ULong = now
    }

    private class CountingHandle : FakeHandle() {
        var closeCalls = 0
        var openHandles = 1
        override fun close() {
            closeCalls++
            openHandles--
        }
    }
}
