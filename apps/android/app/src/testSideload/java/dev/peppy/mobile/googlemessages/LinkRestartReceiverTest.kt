package dev.peppy.mobile.googlemessages

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.Executor
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkRestartReceiverTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Before fun setUp() {
        LinkCoreBridge.recorder = { _, _ -> true }
    }

    @After
    fun cleanUp() {
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks()
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkCoreBridge.resetForTest()
    }

    @Test
    fun `boot completed linked state starts service and finishes pending result`() {
        installHandle(StatusHandle(LINKED_STATUS))
        val starts = mutableListOf<Context>()
        var finishes = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            startService = { starts += it },
            pendingFinished = { finishes += 1 },
        )
        val receiver = receiverWithPendingResult()

        receiver.onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertEquals(1, starts.size)
        assertEquals(1, finishes)
    }

    @Test
    fun `package replaced unlinked state does not start service and finishes pending result`() {
        installHandle(StatusHandle(UNLINKED_STATUS))
        var starts = 0
        var finishes = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            startService = { starts += 1 },
            pendingFinished = { finishes += 1 },
        )
        val receiver = receiverWithPendingResult()

        receiver.onReceive(context, Intent(Intent.ACTION_MY_PACKAGE_REPLACED))

        assertEquals(0, starts)
        assertEquals(1, finishes)
    }

    @Test
    fun `restart republishes terminal states without starting service`() {
        listOf("signed_out", "needs_relink").forEach { terminalState ->
            LinkRuntime.resetForTest()
            LinkCoreBridge.resetForTest()
            val recorded = mutableListOf<String>()
            var starts = 0
            var finishes = 0
            LinkCoreBridge.recorder = { _, status -> recorded += status.state; true }
            installHandle(StatusHandle("{\"state\":\"$terminalState\"}"))
            LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
                executor = Executor(Runnable::run),
                startService = { starts += 1 },
                pendingFinished = { finishes += 1 },
            )

            receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

            assertEquals(listOf("unknown", terminalState), recorded)
            assertEquals(0, starts)
            assertEquals(1, finishes)
        }
    }

    @Test
    fun `unavailable engine reconciles an authoritative empty state without starting the service`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> recorded += status.state; true }
        LinkRuntimeTestHooks.engine = unavailableEngine()
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }
        var starts = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            startService = { starts += 1 },
        )

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertEquals(listOf("unknown", "unlinked"), recorded)
        assertEquals(0, starts)
    }

    @Test
    fun `unavailable engine leaves an unreadable prior state fail closed`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> recorded += status.state; true }
        LinkRuntimeTestHooks.engine = unavailableEngine()
        LinkRuntimeTestHooks.storeFactory = {
            object : LinkStateStorage {
                override fun read(): ByteArray = throw IllegalStateException("link_state_unreadable")
                override fun write(state: ByteArray) = Unit
                override fun wipe() = Unit
            }
        }
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(executor = Executor(Runnable::run))

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertEquals(listOf("unknown"), recorded)
        assertFalse(recorded.contains("unlinked"))
    }

    @Test
    fun `restart advances the observation generation before reading status`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        installHandle(StatusHandle(UNLINKED_STATUS))
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(executor = Executor(Runnable::run))
        val token = LinkCoreBridge.observationToken()

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_MY_PACKAGE_REPLACED))
        assertTrue(LinkCoreBridge.recordObservation(token, LinkRuntime.handleLease(context)!!, context, LinkStatus(0, "unlinked", false, false, false, 0, "")))

        assertEquals(listOf("unknown", "unlinked"), recorded)
    }

    @Test
    fun `status exception for each restart broadcast still finishes pending result`() {
        installHandle(StatusHandle(LINKED_STATUS, failStatus = true))
        var finishes = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            pendingFinished = { finishes += 1 },
        )

        listOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED).forEach { action ->
            receiverWithPendingResult().onReceive(context, Intent(action))
        }

        assertEquals(2, finishes)
    }

    private fun installHandle(handle: LinkHandle) {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }
    }

    private fun unavailableEngine() = object : LinkEngine {
        override val available = false
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("not called")
    }

    private fun receiverWithPendingResult(): LinkRestartReceiver = LinkRestartReceiver().also { receiver ->
        val pending = BroadcastReceiver.PendingResult::class.java.declaredConstructors
            .single { it.parameterCount == 9 }
            .let { constructor ->
                constructor.isAccessible = true
                constructor.newInstance(0, null, null, 0, true, false, null, 0, 0)
            }
        BroadcastReceiver::class.java
            .getDeclaredMethod("setPendingResult", BroadcastReceiver.PendingResult::class.java)
            .invoke(receiver, pending)
    }

    private object EmptyStore : LinkStateStorage {
        override fun read(): ByteArray = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }

    private class StatusHandle(private val value: String, private val failStatus: Boolean = false) : LinkHandle {
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() = Unit
        override fun status(): String = if (failStatus) throw Exception("status_failed") else value
    }

    private companion object {
        const val LINKED_STATUS = """{"state":"linked"}"""
        const val UNLINKED_STATUS = """{"state":"unlinked"}"""
    }
}
