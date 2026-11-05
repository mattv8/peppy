package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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
            assertTrue(controller.begin())
            discovery.pairing.value = 37122
            await(controller) { it.pairingPort == 37122 }
            val code = "123456".toCharArray()
            assertEquals(CodeSubmitResult.ACCEPTED, RestrictedSetupCodeInbox.deliver(code))
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
            assertTrue(code.all { it == '\u0000' })
            awaitNotices(notifier, 4)
            assertEquals(listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.CODE_PROMPT, RestrictedSetupNotice.WORKING, RestrictedSetupNotice.DONE), notifier.notices)
        } finally { scope.cancel() }
    }

    @Test fun `invalid code is zeroed and leaves the inbox waiting`() = runBlocking {
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, FakeDiscovery(), FakeTransport(), FakeNotifier())
            controller.begin()
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
            controller.begin()
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
            controller.begin()
            await(controller) { it.discovery == RestrictedSetupDiscovery.TIMED_OUT }
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
        } finally { scope.cancel() }
    }

    @Test fun `network not allowed hint appears after deadline when wireless debugging is on without connect service`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, FakeNotifier(), FakeWireless(true))
            controller.begin()

            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
        } finally { scope.cancel() }
    }

    @Test fun `connect service clears network not allowed hint and restores searching notice`() = runBlocking {
        val discovery = FakeDiscovery()
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, FakeWireless(true))
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }

            discovery.connect.value = 37123

            await(controller) { it.activeHint == null }
            awaitNotices(notifier, 3)
            assertEquals(
                listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.NETWORK_NOT_ALLOWED, RestrictedSetupNotice.SEARCHING),
                notifier.notices,
            )
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
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("123456".toCharArray()))
            await(controller) { it.hint == RestrictedSetupHint.CODE_REJECTED }
            withTimeout(1_000) { while (!first.closed) delay(1) }
            assertEquals(RestrictedSetupDiscovery.SEARCHING, controller.state.value.discovery)
            assertEquals(null, controller.state.value.pairingPort)
            awaitUntil("CODE_REJECTED notice") { notifier.notices.lastOrNull() == RestrictedSetupNotice.CODE_REJECTED }
            discovery.pairing.value = 37125
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            assertEquals(null, controller.state.value.hint)
            awaitUntil("CODE_PROMPT notice") { notifier.notices.lastOrNull() == RestrictedSetupNotice.CODE_PROMPT }
            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("654321".toCharArray(), 37124))
        } finally { scope.cancel() }
    }

    @Test fun `manual connection port completes after discovery timeout`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, FakeTransport(), FakeNotifier(), RestrictedSetupDeadlines(connectServiceMillis = 1))
            controller.begin()
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
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
            assertEquals(WirelessDebuggingOutcome.REQUESTED, controller.state.value.wirelessDebugging)
            assertEquals(listOf("pair", "connect", "grant-restricted", "grant-blocked", "readback", "off"), transport.events)
        } finally { scope.cancel() }
    }

    @Test fun `partial readback fails verification after both grants`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope, discovery, transport, FakeNotifier(),
                readback = RestrictedSetupReadback(RestrictedSetupAccess.ALLOWED, RestrictedSetupAccess.NOT_ALLOWED),
            )
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.FAILED }
            assertEquals(RestrictedSetupFailure.NOT_VERIFIED, controller.state.value.failure)
            assertEquals(RestrictedSetupAccess.NOT_ALLOWED, controller.state.value.blockedSendersAccess)
            assertEquals(listOf("pair", "connect", "grant-restricted", "grant-blocked", "readback", "off"), transport.events)
        } finally { scope.cancel() }
    }

    @Test fun `first grant only failure uses partial readback and still turns wireless debugging off`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { failAfterFirstGrant = true }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope, discovery, transport, FakeNotifier(),
                readback = RestrictedSetupReadback(RestrictedSetupAccess.ALLOWED, RestrictedSetupAccess.NOT_ALLOWED),
            )

            completeAttempt(controller, discovery)

            assertEquals(RestrictedSetupStage.FAILED, controller.state.value.stage)
            assertEquals(RestrictedSetupFailure.NOT_VERIFIED, controller.state.value.failure)
            assertEquals(RestrictedSetupAccess.ALLOWED, controller.state.value.access)
            assertEquals(RestrictedSetupAccess.NOT_ALLOWED, controller.state.value.blockedSendersAccess)
            assertEquals(WirelessDebuggingOutcome.REQUESTED, controller.state.value.wirelessDebugging)
            assertEquals(listOf("pair", "connect", "grant-restricted", "readback", "off"), transport.events)
        } finally { scope.cancel() }
    }

    @Test fun `unsupported capability is skipped during verification`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope, discovery, transport, FakeNotifier(),
                readback = RestrictedSetupReadback(RestrictedSetupAccess.ALLOWED, RestrictedSetupAccess.UNSUPPORTED),
            )
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }
            assertEquals(RestrictedSetupAccess.UNSUPPORTED, controller.state.value.blockedSendersAccess)
        } finally { scope.cancel() }
    }

    @Test fun `hung grant still reads back and turns wireless debugging off`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { grantDelayMillis = 100 }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier(), RestrictedSetupDeadlines(grantAndReadbackMillis = 1))
            completeAttempt(controller, discovery)
            assertTrue(transport.events.contains("readback"))
            assertTrue(transport.events.contains("off"))
        } finally { scope.cancel() }
    }

    @Test fun `hung readback fails after turning wireless debugging off`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier(), RestrictedSetupDeadlines(grantAndReadbackMillis = 1), readbackDelayMillis = 100)
            completeAttempt(controller, discovery)
            assertEquals(RestrictedSetupFailure.TIMED_OUT, controller.state.value.failure)
            assertTrue(controller.state.value.wirelessDebugging != WirelessDebuggingOutcome.NOT_REQUESTED)
        } finally { scope.cancel() }
    }

    @Test fun `hung turn off completes with wireless debugging still requested`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { turnOffDelayMillis = 100 }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier(), RestrictedSetupDeadlines(turnOffMillis = 1))
            completeAttempt(controller, discovery)
            assertEquals(RestrictedSetupStage.COMPLETE, controller.state.value.stage)
            assertEquals(WirelessDebuggingOutcome.REQUESTED, controller.state.value.wirelessDebugging)
        } finally { scope.cancel() }
    }

    @Test fun `throwing grant leaves readback to decide outcome and turns wireless debugging off`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { grantFails = true }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier())
            completeAttempt(controller, discovery)
            assertEquals(RestrictedSetupStage.COMPLETE, controller.state.value.stage)
            assertTrue(transport.events.containsAll(listOf("readback", "off")))
        } finally { scope.cancel() }
    }

    @Test fun `cancel clears inbox notifier and received code`() = runBlocking {
        val discovery = FakeDiscovery()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val notifier = FakeNotifier()
            val controller = controller(scope, discovery, FakeTransport(), notifier)
            controller.begin()
            controller.cancel()
            val code = "123456".toCharArray()
            assertEquals(CodeSubmitResult.NOT_WAITING, RestrictedSetupCodeInbox.deliver(code))
            assertTrue(code.all { it == '\u0000' })
            assertTrue(notifier.clears > 0)
            assertEquals(RestrictedSetupStage.INTRODUCTION, controller.state.value.stage)
        } finally { scope.cancel() }
    }

    @Test fun `own connect service suppresses network not allowed hint`() = runBlocking {
        val discovery = FakeDiscovery().apply { connect.value = 37123 }
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, wireless, networkMillis = 50)
            controller.begin()
            awaitReads(wireless, 60)
            assertNull(controller.state.value.activeHint)
            assertFalse(notifier.notices.contains(RestrictedSetupNotice.NETWORK_NOT_ALLOWED))
        } finally { scope.cancel() }
    }

    @Test fun `wireless debugging reading off never shows network not allowed hint`() = runBlocking {
        val wireless = FakeWireless(false)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless)
            controller.begin()
            awaitReads(wireless, 10)
            assertNull(controller.state.value.activeHint)
            assertFalse(notifier.notices.contains(RestrictedSetupNotice.NETWORK_NOT_ALLOWED))
        } finally { scope.cancel() }
    }

    @Test fun `unknown wireless debugging setting never shows network not allowed hint`() = runBlocking {
        val wireless = FakeWireless(null)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless)
            controller.begin()
            awaitReads(wireless, 10)
            assertNull(controller.state.value.activeHint)
            assertFalse(notifier.notices.contains(RestrictedSetupNotice.NETWORK_NOT_ALLOWED))
        } finally { scope.cancel() }
    }

    @Test fun `network not allowed hint clears when wireless debugging turns off`() = runBlocking {
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }

            wireless.value = false

            await(controller) { it.activeHint == null }
            awaitNotices(notifier, 3)
            assertEquals(
                listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.NETWORK_NOT_ALLOWED, RestrictedSetupNotice.SEARCHING),
                notifier.notices,
            )
        } finally { scope.cancel() }
    }

    @Test fun `wireless debugging turned on after the deadline with a quick connect service never shows the hint`() = runBlocking {
        val discovery = FakeDiscovery()
        val wireless = FakeWireless(false)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, wireless, networkMillis = 20, checkMillis = 5)
            controller.begin()
            awaitReads(wireless, 10)

            discovery.connect.value = 37123
            wireless.value = true
            val readsAtOn = wireless.reads.get()
            awaitReads(wireless, readsAtOn + 20)

            assertNull(controller.state.value.activeHint)
            assertFalse(notifier.notices.contains(RestrictedSetupNotice.NETWORK_NOT_ALLOWED))
        } finally { scope.cancel() }
    }

    @Test fun `a single negative check does not show network not allowed hint`() = runBlocking {
        val wireless = FakeWireless().apply { script = { read -> read == 1 } }
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless, networkMillis = 0)
            controller.begin()
            awaitReads(wireless, 10)
            assertNull(controller.state.value.activeHint)
            assertFalse(notifier.notices.contains(RestrictedSetupNotice.NETWORK_NOT_ALLOWED))
        } finally { scope.cancel() }
    }

    @Test fun `no wifi takes precedence over network not allowed in state and notices`() = runBlocking {
        val discovery = FakeDiscovery()
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        lateinit var subject: RestrictedSetupController
        // Hold the network check until NO_WIFI is set, so the ordering does not depend on scheduling.
        val wireless = RestrictedSetupWirelessReadback {
            while (subject.state.value.hint != RestrictedSetupHint.NO_WIFI) delay(1)
            true
        }
        try {
            subject = controller(scope, discovery, FakeTransport(), notifier, RestrictedSetupDeadlines(networkAllowedHintMillis = 1, networkAllowedHintCheckMillis = 1), wireless = wireless)
            subject.begin()
            await(subject) { it.hint == RestrictedSetupHint.NO_WIFI }
            await(subject) { it.networkNotAllowed }
            assertEquals(RestrictedSetupHint.NO_WIFI, subject.state.value.activeHint)
            assertEquals(listOf(RestrictedSetupNotice.SEARCHING), notifier.notices)
        } finally { scope.cancel() }
    }

    @Test fun `cancel stops network not allowed checks and notices`() = runBlocking {
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
            awaitNotices(notifier, 2)

            controller.cancel()
            settle(20)
            val reads = wireless.reads.get()
            val noticesAtCancel = notifier.notices.size
            settle(50)

            assertEquals(reads, wireless.reads.get())
            assertEquals(noticesAtCancel, notifier.notices.size)
            assertEquals(RestrictedSetupStage.INTRODUCTION, controller.state.value.stage)
        } finally { scope.cancel() }
    }

    @Test fun `leaving awaiting pairing stops network not allowed checks and notices`() = runBlocking {
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, FakeDiscovery(), notifier, wireless)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
            awaitNotices(notifier, 2)

            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("123456".toCharArray(), 37122))
            await(controller) { it.stage != RestrictedSetupStage.AWAITING_PAIRING }
            settle(20)
            val reads = wireless.reads.get()
            val noticesAfterLeaving = notifier.notices.toList()
            settle(50)

            assertEquals(reads, wireless.reads.get())
            assertEquals(noticesAfterLeaving, notifier.notices.toList())
            assertEquals(1, notifier.notices.count { it == RestrictedSetupNotice.NETWORK_NOT_ALLOWED })
        } finally { scope.cancel() }
    }

    @Test fun `rejected code clears network not allowed hint and shows code rejected`() = runBlocking {
        val discovery = FakeDiscovery()
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val transport = FakeTransport().apply { rejectPair = true; onPair = { wireless.value = false } }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, wireless, transport = transport)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }

            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("123456".toCharArray()))
            await(controller) { it.stage == RestrictedSetupStage.AWAITING_PAIRING && it.hint == RestrictedSetupHint.CODE_REJECTED }
            assertEquals(RestrictedSetupHint.CODE_REJECTED, controller.state.value.activeHint)
            assertFalse(controller.state.value.networkNotAllowed)

            awaitReads(wireless, wireless.reads.get() + 3)
            assertEquals(RestrictedSetupHint.CODE_REJECTED, controller.state.value.activeHint)
            awaitUntil("CODE_REJECTED notice") { notifier.notices.lastOrNull() == RestrictedSetupNotice.CODE_REJECTED }
            assertEquals(1, notifier.notices.count { it == RestrictedSetupNotice.NETWORK_NOT_ALLOWED })
        } finally { scope.cancel() }
    }

    @Test fun `transport teardown failure before completion fails with transport failure and blocks retries`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { rejectPair = true; closeFails = true }
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, notifier)
            assertTrue(controller.begin())
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            assertEquals(CodeSubmitResult.ACCEPTED, controller.submitCode("123456".toCharArray()))

            await(controller) { it.stage == RestrictedSetupStage.FAILED && it.retryBlocked }
            assertEquals(RestrictedSetupFailure.TRANSPORT, controller.state.value.failure)
            awaitUntil("FAILED notice") { notifier.notices.contains(RestrictedSetupNotice.FAILED) }
            assertFalse(controller.begin())
            assertNull(RestrictedSetupCodeInbox.sink)
        } finally { scope.cancel() }
    }

    @Test fun `transport teardown failure after completion keeps complete stage and blocks retries`() = runBlocking {
        val discovery = FakeDiscovery()
        val transport = FakeTransport().apply { closeFails = true }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, transport, FakeNotifier())
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            controller.submitCode("123456".toCharArray())
            discovery.connect.value = 37123
            await(controller) { it.stage == RestrictedSetupStage.COMPLETE }

            await(controller) { it.retryBlocked }
            assertEquals(RestrictedSetupStage.COMPLETE, controller.state.value.stage)
            assertNull(controller.state.value.failure)
            assertFalse(controller.begin())
        } finally { scope.cancel() }
    }

    @Test fun `hint clearing with a pairing port already found does not duplicate code prompt`() = runBlocking {
        val discovery = FakeDiscovery()
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, wireless)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            awaitNotices(notifier, 3)

            discovery.connect.value = 37123
            await(controller) { it.activeHint == null }
            awaitReads(wireless, wireless.reads.get() + 3)

            assertEquals(
                listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.NETWORK_NOT_ALLOWED, RestrictedSetupNotice.CODE_PROMPT),
                notifier.notices,
            )
        } finally { scope.cancel() }
    }

    @Test fun `show code prompt again reposts code prompt even when it was the last notice`() = runBlocking {
        val discovery = FakeDiscovery()
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, discovery, FakeTransport(), notifier)
            controller.begin()
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            awaitNotices(notifier, 2)

            controller.showCodePromptAgain()

            awaitNotices(notifier, 3)
            assertEquals(
                listOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.CODE_PROMPT, RestrictedSetupNotice.CODE_PROMPT),
                notifier.notices,
            )
        } finally { scope.cancel() }
    }

    @Test fun `code prompt is posted again after an invalid code notice`() = runBlocking {
        val discovery = FakeDiscovery()
        val wireless = FakeWireless(true)
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = networkController(scope, discovery, notifier, wireless)
            controller.begin()
            await(controller) { it.activeHint == RestrictedSetupHint.NETWORK_NOT_ALLOWED }
            discovery.pairing.value = 37122
            await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
            awaitNotices(notifier, 3)

            assertEquals(CodeSubmitResult.INVALID_FORMAT, controller.submitCode("12x456".toCharArray(), 37122))
            awaitNotices(notifier, 4)

            discovery.connect.value = 37123
            await(controller) { it.activeHint == null }
            awaitNotices(notifier, 5)
            assertEquals(
                listOf(
                    RestrictedSetupNotice.SEARCHING,
                    RestrictedSetupNotice.NETWORK_NOT_ALLOWED,
                    RestrictedSetupNotice.CODE_PROMPT,
                    RestrictedSetupNotice.CODE_INVALID,
                    RestrictedSetupNotice.CODE_PROMPT,
                ),
                notifier.notices,
            )
        } finally { scope.cancel() }
    }

    @Test fun `foreground unavailable fails with transport failure without blocking retries`() = runBlocking {
        val notifier = FakeNotifier()
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, FakeDiscovery(), FakeTransport(), notifier)
            assertTrue(controller.begin())

            controller.reportForegroundUnavailable()

            assertEquals(RestrictedSetupStage.FAILED, controller.state.value.stage)
            assertEquals(RestrictedSetupFailure.TRANSPORT, controller.state.value.failure)
            assertFalse(controller.state.value.retryBlocked)
            assertEquals(RestrictedSetupNotice.FAILED, notifier.notices.last())
            assertTrue(controller.begin())
            controller.cancel()
        } finally { scope.cancel() }
    }

    private fun controller(
        scope: CoroutineScope,
        discovery: FakeDiscovery,
        transport: FakeTransport,
        notifier: FakeNotifier,
        deadlines: RestrictedSetupDeadlines = RestrictedSetupDeadlines(),
        factory: RestrictedSetupTransportFactory = RestrictedSetupTransportFactory { transport },
        wireless: RestrictedSetupWirelessReadback = RestrictedSetupWirelessReadback { null },
        addresses: Set<InetAddress> = setOf(loopback),
        readback: RestrictedSetupReadback = RestrictedSetupReadback(RestrictedSetupAccess.ALLOWED, RestrictedSetupAccess.ALLOWED),
        readbackDelayMillis: Long = 0,
    ) = RestrictedSetupController(
        scope, factory, RestrictedSetupAccessReadback { transport.events += "readback"; if (readbackDelayMillis > 0) delay(readbackDelayMillis); readback }, LocalAddressSupplier { addresses }, discovery, notifier, wireless, deadlines,
    )

    /** LAN address (not loopback), so NO_WIFI does not interfere with network-hint tests. */
    private fun networkController(
        scope: CoroutineScope,
        discovery: FakeDiscovery,
        notifier: FakeNotifier,
        wireless: FakeWireless,
        networkMillis: Long = 1,
        checkMillis: Long = 1,
        transport: FakeTransport = FakeTransport(),
    ) = controller(
        scope, discovery, transport, notifier,
        RestrictedSetupDeadlines(networkAllowedHintMillis = networkMillis, networkAllowedHintCheckMillis = checkMillis),
        wireless = wireless,
        addresses = setOf(lan),
    )

    private suspend fun await(controller: RestrictedSetupController, predicate: (RestrictedSetupState) -> Boolean) = try {
        withTimeout(5_000) { while (!predicate(controller.state.value)) delay(1) }
    } catch (_: TimeoutCancellationException) {
        fail("Timed out in state ${controller.state.value}")
    }

    private suspend fun completeAttempt(controller: RestrictedSetupController, discovery: FakeDiscovery) {
        controller.begin()
        discovery.pairing.value = 37122
        await(controller) { it.discovery == RestrictedSetupDiscovery.FOUND }
        controller.submitCode("123456".toCharArray())
        discovery.connect.value = 37123
        await(controller) { it.stage == RestrictedSetupStage.COMPLETE || it.stage == RestrictedSetupStage.FAILED }
    }

    private suspend fun awaitUntil(description: String, predicate: () -> Boolean) = try {
        withTimeout(5_000) { while (!predicate()) delay(1) }
    } catch (_: TimeoutCancellationException) {
        fail("Timed out waiting for $description")
    }

    private suspend fun awaitNotices(notifier: FakeNotifier, count: Int) = awaitUntil("$count notices") { notifier.notices.size >= count }
    private suspend fun awaitReads(wireless: FakeWireless, count: Int) = awaitUntil("$count wireless reads") { wireless.reads.get() >= count }

    /** Bounded absence window for asserting that something stopped; a condition cannot prove absence. */
    private suspend fun settle(millis: Long) {
        try { withTimeout(millis) { while (true) delay(1) } } catch (_: TimeoutCancellationException) { }
    }

    private class FakeDiscovery : AdbServiceDiscovery {
        val pairing = MutableStateFlow<Int?>(null)
        val connect = MutableStateFlow<Int?>(null)
        override fun ports(kind: AdbServiceKind): Flow<Int?> = if (kind == AdbServiceKind.PAIRING) pairing else connect
    }

    private class FakeNotifier : RestrictedSetupNotifier {
        val notices = CopyOnWriteArrayList<RestrictedSetupNotice>()
        @Volatile var clears = 0
        override fun canPrompt() = true
        override fun show(notice: RestrictedSetupNotice) { notices += notice }
        override fun clear() { clears++ }
    }

    private class FakeWireless(@Volatile var value: Boolean? = null) : RestrictedSetupWirelessReadback {
        val reads = AtomicInteger()
        @Volatile var script: ((Int) -> Boolean?)? = null
        override suspend fun isOn(): Boolean? {
            val read = reads.incrementAndGet()
            val scripted = script
            return if (scripted != null) scripted(read) else value
        }
    }

    private class FakeTransport : RestrictedSetupTransport {
        val events = mutableListOf<String>()
        var rejectPair = false
        var turnOffFails = false
        var grantFails = false
        var failAfterFirstGrant = false
        var grantDelayMillis = 0L
        var turnOffDelayMillis = 0L
        var closeFails = false
        var onPair: () -> Unit = {}
        @Volatile var closed = false
        val closeAttempts = AtomicInteger()
        override suspend fun pair(endpoint: LocalEndpoint, code: CharArray) { events += "pair"; onPair(); if (rejectPair) throw PairingRejectedException() }
        override suspend fun connect(endpoint: LocalEndpoint) { events += "connect" }
        override suspend fun grantAdvancedFeatures() {
            events += "grant-restricted"
            if (failAfterFirstGrant) throw java.io.IOException("Connection lost after first grant")
            events += "grant-blocked"
            if (grantDelayMillis > 0) delay(grantDelayMillis)
            if (grantFails) error("failed")
        }
        override suspend fun turnWirelessDebuggingOff() { events += "off"; if (turnOffDelayMillis > 0) delay(turnOffDelayMillis); if (turnOffFails) error("failed") }
        override fun close() {
            closeAttempts.incrementAndGet()
            closed = true
            if (closeFails) error("close failed")
        }
    }

    private companion object {
        val loopback: InetAddress = InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
        val lan: InetAddress = InetAddress.getByAddress(byteArrayOf(192.toByte(), 168.toByte(), 1, 2))
    }
}
