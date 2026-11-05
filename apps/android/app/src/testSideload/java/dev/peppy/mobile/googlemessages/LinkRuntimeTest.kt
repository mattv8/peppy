package dev.peppy.mobile.googlemessages

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.nio.file.Files
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import dev.peppy.mobile.security.AesGcmSecretBox
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
    private val directory = Files.createTempDirectory("link-runtime").toFile()

    @After
    fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkRuntimeTestHooks.eventRecorder = null
        directory.deleteRecursively()
    }

    @Test
    fun `handle opens once`() {
        val handle = FakeHandle()
        var opens = 0
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle.also { opens++ }
        }
        LinkRuntimeTestHooks.storeFactory = { store() }

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
        File(directory, "google-messages-link-state.bin").writeBytes(byteArrayOf(1))
        LinkRuntimeTestHooks.engine = availableEngine(FakeHandle())
        LinkRuntimeTestHooks.storeFactory = { LinkStateStore(directory) }

        val result = LinkRuntime.handle(context)

        assertTrue(result is LinkOpenResult.Unreadable)
        assertTrue(File(directory, "google-messages-link-state.bin").isFile)
    }

    @Test
    fun `reset stops closes wipes and records a fixed event`() {
        val handle = FakeHandle()
        val events = mutableListOf<String>()
        val box = AesGcmSecretBox(keyProvider())
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { LinkStateStore(directory, box) }
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> events += "$kind/$code" }
        val store = LinkStateStore(directory, box)
        store.write(byteArrayOf(1))
        LinkRuntime.handle(context)

        LinkRuntime.resetLocalState(context)

        assertTrue(handle.closed)
        assertFalse(File(directory, "google-messages-link-state.bin").exists())
        assertEquals(listOf("link/reset_local"), events)
    }

    @Test
    fun `startup creates link notification channel`() {
        GoogleMessagesLink.onAppStart(context)

        val manager = context.getSystemService(NotificationManager::class.java)
        assertEquals(NotificationManager.IMPORTANCE_LOW, manager.getNotificationChannel(GoogleMessagesLinkService.CHANNEL_ID).importance)
    }

    private fun store(): LinkStateStore = LinkStateStore(directory, AesGcmSecretBox(keyProvider()))

    private fun availableEngine(handle: LinkHandle) = object : LinkEngine {
        override val available = true
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
    }

    private fun keyProvider(): (Boolean) -> SecretKey? {
        var key: SecretKey? = null
        return { create -> key ?: if (create) KeyGenerator.getInstance("AES").apply { init(256) }.generateKey().also { key = it } else null }
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
