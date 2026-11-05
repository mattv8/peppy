package dev.peppy.mobile.googlemessages

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
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
    }

    @Test
    fun `startup creates link notification channel`() {
        GoogleMessagesLink.onAppStart(context)

        val manager = context.getSystemService(NotificationManager::class.java)
        assertEquals(NotificationManager.IMPORTANCE_LOW, manager.getNotificationChannel(GoogleMessagesLinkService.CHANNEL_ID).importance)
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
