package dev.peppy.mobile.googlemessages

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkRuntimeTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @After
    fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkRuntimeTestHooks.eventRecorder = null
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

        assertTrue(LinkRuntime.beginPairing())
        host.showEmoji("first")
        host.event("link", "pair_ok", 0)
        assertTrue(first.await(1, TimeUnit.SECONDS))

        assertTrue(LinkRuntime.beginPairing())

        assertEquals(null, LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.pairingInProgress.get())
        LinkRuntime.endPairing()
        assertEquals(null, LinkRuntime.emoji.value)
        assertFalse(LinkRuntime.pairingInProgress.get())
    }

    @Test
    fun `second owner cannot clear an active pairing`() {
        assertTrue(LinkRuntime.beginPairing())
        LinkRuntime.emoji.value = "first"

        assertFalse(LinkRuntime.beginPairing())

        assertEquals("first", LinkRuntime.emoji.value)
        assertTrue(LinkRuntime.pairingInProgress.get())
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
    fun `runtime event sink records through the production diagnostic path`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.configureRuntime(enabled = true) { records += it }

        LinkRuntimeEventSink.record("link", "reset_local", 0)

        assertTrue(records.single().contains("event=27"))
        assertTrue(records.single().contains("29=17"))
        SyncDiagnostics.configureRuntime(enabled = false) {}
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

    private class FakeHandle : LinkHandle {
        var closed = false
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() { closed = true }
        override fun status(): String = """{\"state\":\"unlinked\"}"""
    }
}
