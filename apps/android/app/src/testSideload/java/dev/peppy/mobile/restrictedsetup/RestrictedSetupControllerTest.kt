package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class RestrictedSetupControllerTest {
    @Test fun `discovery finds a pairing port and notification reply completes setup`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport()
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, notifier)
            assertTrue(controller.begin(RestrictedSetupOptions()))
            discovery.pairing.value = 37122
            await(controller) { it.pairingPort == 37122 }
            val code = "123456".toCharArray()
            assertEquals(CodeSubmitResult.ACCEPTED, RestrictedSetupCodeInbox.deliver(code))
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
            assertTrue(code.all { it == '\u0000' })
            assertEquals(listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.CODE_PROMPT, RestrictedSetupNotice.WORKING, RestrictedSetupNotice.DONE), notifier.notices)
        } finally { scope.cancel() }
    }

    @Test fun `invalid code is zeroed and leaves the inbox waiting`() = runBlocking {
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, FakeDiscovery(), FakeTransport(), FakeNotifier())
            controller.begin(RestrictedSetupOptions())
            val code = "12x456".toCharArray()
            assertEquals(CodeSubmitResult.INVALID_FORMAT, controller.submitCode(code, 37122))
            assertTrue(code.all { it == '\u0000' })
            assertEquals(RestrictedSetupStage.AWAITING_PAIRING, controller.state.value.stage)
        } finally { scope.cancel() }
    }

    @Test fun `lost box returns to searching with its hint`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, FakeTransport(), FakeNotifier())
            controller.begin(RestrictedSetupOptions())
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            discovery.pairing.value = null
            await(controller) { it.hint == RestrictedSetupHint.PAIRING_BOX_CLOSED }
            assertEquals(RestrictedSetupDiscovery.SEARCHING, controller.state.value.discovery)
        } finally { scope.cancel() }
    }

    @Test fun `discovery timeout keeps listening for a late pairing box`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, FakeTransport(), FakeNotifier(), RestrictedSetupDeadlines(pairingServiceHintMillis = 1))
            controller.begin(RestrictedSetupOptions())
            await(controller) { it.discovery == RestrictedSetupDiscovery.TIMED_OUT }
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
        } finally { scope.cancel() }
    }

    @Test fun `rejected code closes its transport and permits a new transport`() = runBlocking {
        val discovery = FakeDiscovery()
        val first = FakeTransport().apply { rejectPair = true }
        val second = FakeTransport()
        var creates = 0
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val notifier = FakeNotifier()
            val controller = controller(scope, discovery, first, notifier, factory = RestrictedSetupTransportFactory { if (creates++ == 0) first else second })
            controller.begin(RestrictedSetupOptions())
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("123456".toCharArray()))
            await(controller) { it.hint == RestrictedSetupHint.CODE_REJECTED }
            withTimeout(1_000) { while (!first.closed) delay(1) }
            assertEquals(RestrictedSetupDiscovery.SEARCHING, controller.state.value.discovery)
            assertEquals(null, controller.state.value.pairingPort)
            assertEquals(RestrictedSetupNotice.CODE_REJECTED, notifier.notices.last())
            discovery.pairing.value = 37125
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            assertEquals(null, controller.state.value.hint)
            assertEquals(RestrictedSetupNotice.CODE_PROMPT, notifier.notices.last())
            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("654321".toCharArray(), 37124))
        } finally { scope.cancel() }
    }

    @Test fun `manual connection port completes after discovery timeout`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, FakeTransport(), FakeNotifier(), RestrictedSetupDeadlines(connectServiceMillis = 1))
            controller.begin(RestrictedSetupOptions())
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            await(controller) { it.needsConnectPort }
            assertTrue(controller.submitConnectPort(37123))
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
        } finally { scope.cancel() }
    }

    @Test fun `wireless debugging is turned off only after allowed readback and failures are nonfatal`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { turnOffFails = true }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier())
            controller.begin(RestrictedSetupOptions(turnOffWirelessDebugging = true))
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
            assertEquals(WirelessDebuggingOutcome.REQUESTED, controller.state.value.wirelessDebugging)
            assertEquals(listOf("pair", "connect", "grant", "readback", "off"), transport.events)
        } finally { scope.cancel() }
    }

    @Test fun `cancel clears inbox notifier and received code`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val notifier = FakeNotifier()
            val controller = controller(scope, discovery, FakeTransport(), notifier)
            controller.begin(RestrictedSetupOptions())
            controller.cancel()
            val code = "123456".toCharArray()
            assertEquals(CodeSubmitResult.NOT_WAITING, RestrictedSetupCodeInbox.deliver(code))
            assertTrue(code.all { it == '\u0000' })
            assertTrue(notifier.clears > 0)
            assertEquals(RestrictedSetupStage.INTRODUCTION, controller.state.value.stage)
        } finally { scope.cancel() }
    }

    private fun controller(scope: CoroutineScope, discovery: FakeDiscovery, transport: FakeTransport, notifier: FakeNotifier, deadlines: RestrictedSetupDeadlines = RestrictedSetupDeadlines(), factory: RestrictedSetupTransportFactory = RestrictedSetupTransportFactory { transport }) = RestrictedSetupController(
        scope, factory, RestrictedSetupAccessReadback { transport.events += "readback"; true }, LocalAddressSupplier { setOf(loopback) }, Marker(), discovery, notifier, RestrictedSetupWirelessReadback { null }, deadlines,
    )

    private suspend fun await(controller: RestrictedSetupController, predicate: (RestrictedSetupState) -> Boolean) = try {
        withTimeout(1_000) { while (!predicate(controller.state.value)) delay(1) }
    } catch (error: kotlinx.coroutines.TimeoutCancellationException) {
        fail("Timed out in state ${controller.state.value}")
    }

    private class FakeDiscovery : AdbServiceDiscovery {
        val pairing = MutableStateFlow<Int?>(null)
        val connect = MutableStateFlow<Int?>(null)
        override fun ports(kind: AdbServiceKind): Flow<Int?> = if (kind == AdbServiceKind.PAIRING) pairing else connect
    }
    private class Marker : RestrictedSetupCleanupMarker { private var needed = false; override fun needsCleanup() = needed; override fun markCleanupNeeded() = true.also { needed = true }; override fun acknowledgeCleanup() = true.also { needed = false } }
    private class FakeNotifier : RestrictedSetupNotifier { val notices = mutableListOf<RestrictedSetupNotice>(); var clears = 0; override fun canPrompt() = true; override fun show(notice: RestrictedSetupNotice) { notices += notice }; override fun clear() { clears++ } }
    private class FakeTransport : RestrictedSetupTransport {
        val events = mutableListOf<String>(); var rejectPair = false; var turnOffFails = false; @Volatile var closed = false
        override suspend fun pair(endpoint: LocalEndpoint, code: CharArray) { events += "pair"; if (rejectPair) throw PairingRejectedException() }
        override suspend fun connect(endpoint: LocalEndpoint) { events += "connect" }
        override suspend fun grantOwnRestrictedAccess() { events += "grant" }
        override suspend fun turnWirelessDebuggingOff() { events += "off"; if (turnOffFails) error("failed") }
        override fun close() { closed = true }
    }
    private companion object { val loopback: InetAddress = InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1)) }
}
