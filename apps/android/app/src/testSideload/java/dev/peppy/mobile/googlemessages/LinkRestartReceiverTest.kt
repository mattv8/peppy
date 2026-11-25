package dev.peppy.mobile.googlemessages

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.peppy.mobile.gateway.GatewayTestBase
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadow.api.Shadow
import org.robolectric.shadows.ShadowBroadcastPendingResult
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.NativeRcsLinkStatus

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = LinkTestApplication::class)
class LinkRestartReceiverTest : GatewayTestBase() {
    private lateinit var client: NativeClient

    @Before fun setUp() {
        client = enrollAndUnlock()
    }

    @After
    fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkCoreBridge.resetForTest()
    }

    @Test
    fun `boot completed linked state starts service and finishes pending result`() {
        installHandle(StatusHandle(LINKED_STATUS))
        val starts = mutableListOf<Context>()
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        LinkTestDeps.startService = { starts += it }
        val pending = newPendingResult()

        receiverWithPendingResult(pending).onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertEquals(1, starts.size)
        assertTrue(isFinished(pending))
    }

    @Test
    fun `package replaced unlinked state does not start service and finishes pending result`() {
        installHandle(StatusHandle(UNLINKED_STATUS))
        var starts = 0
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        LinkTestDeps.startService = { starts += 1 }
        val pending = newPendingResult()

        receiverWithPendingResult(pending).onReceive(context, Intent(Intent.ACTION_MY_PACKAGE_REPLACED))

        assertEquals(0, starts)
        assertTrue(isFinished(pending))
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
        LinkTestDeps.engine = unavailableEngine()
        LinkTestDeps.storeFactory = { ObservedStore { atRead = lifecycleSnapshot() } }
        var starts = 0
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        LinkTestDeps.startService = { starts += 1 }

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNLINKED, 2uL, true)
        assertEquals(0, starts)
    }

    @Test
    fun `unavailable engine leaves an unreadable prior state fail closed`() {
        var atRead: LifecycleSnapshot? = null
        LinkTestDeps.engine = unavailableEngine()
        LinkTestDeps.storeFactory = {
            ObservedStore {
                atRead = lifecycleSnapshot()
                throw IllegalStateException("link_state_unreadable")
            }
        }
        var starts = 0
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        LinkTestDeps.startService = { starts += 1 }

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertEquals(0, starts)
    }

    @Test
    fun `restart advances the observation generation before reading status`() {
        installHandle(StatusHandle(UNLINKED_STATUS))
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        val token = LinkCoreBridge.observationToken()

        receiverWithPendingResult().onReceive(context, Intent(Intent.ACTION_MY_PACKAGE_REPLACED))
        assertEquals(token + 1, LinkCoreBridge.observationToken())
        assertTrue(LinkCoreBridge.recordObservation(token, LinkRuntime.handleLease(context)!!, context, LinkStatus(0, "linked", false, false, false, 0, "")))

        assertLifecycle(lifecycleSnapshot(), NativeRcsLinkState.UNLINKED, 2uL, true)
    }

    @Test
    fun `status exception for each restart broadcast still finishes pending result`() {
        installHandle(StatusHandle(LINKED_STATUS, failStatus = true))
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        val pendings = listOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED).map { action ->
            newPendingResult().also { pending -> receiverWithPendingResult(pending).onReceive(context, Intent(action)) }
        }

        assertTrue(pendings.all { isFinished(it) })
    }

    @Test
    fun `rejected restart executor finishes pending result without starting service`() {
        installHandle(StatusHandle(LINKED_STATUS))
        var starts = 0
        LinkTestDeps.restartExecutor = Executor { throw RejectedExecutionException("restart_rejected") }
        LinkTestDeps.startService = { starts += 1 }
        val pending = newPendingResult()

        receiverWithPendingResult(pending).onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertEquals(0, starts)
        assertTrue(isFinished(pending))
    }

    private fun assertTerminalRestart(terminalState: String, expected: NativeRcsLinkState) {
        var atRead: LifecycleSnapshot? = null
        var starts = 0
        installHandle(StatusHandle("{\"state\":\"$terminalState\"}") { atRead = lifecycleSnapshot() })
        LinkTestDeps.restartExecutor = Executor(Runnable::run)
        LinkTestDeps.startService = { starts += 1 }
        val pending = newPendingResult()

        receiverWithPendingResult(pending).onReceive(context, Intent(Intent.ACTION_BOOT_COMPLETED))

        assertLifecycle(atRead, NativeRcsLinkState.UNAVAILABLE, 1uL, false)
        assertLifecycle(lifecycleSnapshot(), expected, 2uL, true)
        assertEquals(0, starts)
        assertTrue(isFinished(pending))
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
        LinkTestDeps.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        LinkTestDeps.storeFactory = { EmptyStore }
    }

    private fun unavailableEngine() = object : LinkEngine {
        override val available = false
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("not called")
    }

    private fun receiverWithPendingResult(pending: BroadcastReceiver.PendingResult = newPendingResult()): LinkRestartReceiver =
        LinkRestartReceiver().also { receiver ->
            BroadcastReceiver::class.java
                .getDeclaredMethod("setPendingResult", BroadcastReceiver.PendingResult::class.java)
                .invoke(receiver, pending)
        }

    private fun newPendingResult(): BroadcastReceiver.PendingResult {
        val constructor = BroadcastReceiver.PendingResult::class.java.declaredConstructors
            .single { it.parameterCount == 9 }
        constructor.isAccessible = true
        return constructor.newInstance(0, null, null, 0, true, false, null, 0, 0) as BroadcastReceiver.PendingResult
    }

    /** True once the real PendingResult.finish() has run; Robolectric's shadow completes this future there. */
    private fun isFinished(pending: BroadcastReceiver.PendingResult): Boolean =
        Shadow.extract<ShadowBroadcastPendingResult>(pending).getFuture().isDone

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
