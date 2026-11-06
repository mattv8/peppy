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
        val handle = FakeHandle { generation, revision -> generation == currentGeneration && revision == 4L }
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
    ) : LinkHandle {
        var closed = false
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() { closed = true }
        override fun status(): String = """{\"state\":\"unlinked\"}"""
        override fun acceptsOutput(generation: Long, revision: Long): Boolean = accepts(generation, revision)
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

    private class CountingHandle : FakeHandle() {
        var closeCalls = 0
        var openHandles = 1
        override fun close() {
            closeCalls++
            openHandles--
        }
    }
}
