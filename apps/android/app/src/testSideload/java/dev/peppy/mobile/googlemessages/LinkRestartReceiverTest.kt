package dev.peppy.mobile.googlemessages

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.peppy.mobile.gateway.GatewayTestBase
import java.util.concurrent.Executor
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.NativeRcsLinkStatus

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkRestartReceiverTest : GatewayTestBase() {
    private lateinit var client: NativeClient

    @Before fun setUp() {
        client = enrollAndUnlock()
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
    fun `restart republishes signed out without starting service`() {
        assertTerminalRestart("signed_out", NativeRcsLinkState.SIGNED_OUT)
    }

    @Test
    fun `restart republishes needs relink without starting service`() {
        assertTerminalRestart("needs_relink", NativeRcsLinkState.NEEDS_RELINK)
    }

    @Test
    fun `unavailable engine reconciles an authoritative empty state without starting the service`() {
        var atRead: LifecycleSnapshot? = null
        LinkRuntimeTestHooks.engine = unavailableEngine()
        LinkRuntimeTestHooks.storeFactory = { ObservedStore { atRead = lifecycleSnapshot() } }
        var starts = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            startService = { starts += 1 },
        )

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNLINKED, 2uL, true)
        assertEquals(0, starts)
    }

    @Test
    fun `unavailable engine leaves an unreadable prior state fail closed`() {
        var atRead: LifecycleSnapshot? = null
        LinkRuntimeTestHooks.engine = unavailableEngine()
        LinkRuntimeTestHooks.storeFactory = {
            ObservedStore {
                atRead = lifecycleSnapshot()
                throw IllegalStateException("link_state_unreadable")
            }
        }
        var starts = 0
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(executor = Executor(Runnable::run), startService = { starts += 1 })

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertEquals(0, starts)
    }

    @Test
    fun `restart advances the observation generation before reading status`() {
        installHandle(StatusHandle(UNLINKED_STATUS))
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(executor = Executor(Runnable::run))
        val token = LinkCoreBridge.observationToken()

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_MY_PACKAGE_REPLACED))
        assertEquals(token + 1, LinkCoreBridge.observationToken())
        assertTrue(LinkCoreBridge.recordObservation(token, LinkRuntime.handleLease(context)!!, context, LinkStatus(0, "linked", false, false, false, 0, "")))

        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNLINKED, 2uL, true)
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

    private fun assertTerminalRestart(terminalState: String, expected: NativeRcsLinkState) {
        var atRead: LifecycleSnapshot? = null
        var starts = 0
        var finishes = 0
        installHandle(StatusHandle("{\"state\":\"$terminalState\"}") { atRead = lifecycleSnapshot() })
        LinkRestartReceiver.testHooks = RestartReceiverTestHooks(
            executor = Executor(Runnable::run),
            startService = { starts += 1 },
            pendingFinished = { finishes += 1 },
        )

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), expected, 2uL, true)
        assertEquals(0, starts)
        assertEquals(1, finishes)
    }

    private fun lifecycleSnapshot() = LifecycleSnapshot(
        client.rcsLinkStatus(deviceId, System.currentTimeMillis()),
        client.gatewayRcsLinkLifecycleGeneration(),
    )

    private fun assertLifecycle(snapshot: LifecycleSnapshot?, state: NativeRcsLinkState, generation: ULong, availability: Boolean) {
        assertEquals(state, snapshot?.status?.state)
        assertEquals(generation, snapshot?.generation)
        assertEquals(availability, snapshot?.status?.availability)
    }

    private data class LifecycleSnapshot(val status: NativeRcsLinkStatus?, val generation: ULong?)

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

    private class ObservedStore(private val onRead: () -> Unit) : LinkStateStorage {
        override fun read(): ByteArray {
            onRead()
            return ByteArray(0)
        }
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }

    private class StatusHandle(private val value: String, private val failStatus: Boolean = false, private val onStatus: () -> Unit = {}) : LinkHandle {
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() = Unit
        override fun status(): String {
            onStatus()
            return if (failStatus) throw Exception("status_failed") else value
        }
    }

    private companion object {
        const val LINKED_STATUS = """{"state":"linked"}"""
        const val UNLINKED_STATUS = """{"state":"unlinked"}"""
    }
}
