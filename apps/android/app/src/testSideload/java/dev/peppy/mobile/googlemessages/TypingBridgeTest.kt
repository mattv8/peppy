package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.googlemessages.live.LiveScheduledTask
import dev.peppy.mobile.googlemessages.live.LiveScheduler
import dev.peppy.mobile.googlemessages.live.LiveClock
import dev.peppy.mobile.googlemessages.live.LiveTypingCredentials
import dev.peppy.mobile.googlemessages.live.LiveTypingDisabledReason
import dev.peppy.mobile.googlemessages.live.LiveTypingEnableContext
import dev.peppy.mobile.googlemessages.live.LiveTypingHost
import dev.peppy.mobile.googlemessages.live.LiveTypingListener
import java.time.Duration
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import okhttp3.Request
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowSystemClock
import uniffi.peppy_mobile_bindings.NativeLiveAction
import uniffi.peppy_mobile_bindings.NativeLiveClock
import uniffi.peppy_mobile_bindings.NativeLiveEnableContext
import uniffi.peppy_mobile_bindings.NativeLiveOutbound
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeLiveSessionUpdate
import uniffi.peppy_mobile_bindings.NativeTypingState

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class TypingBridgeTest {
    @Test fun `enable applies core sharing then activates ordered outbound delivery`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "ok" }

        bridge.start(credentials())
        transport.enable(listOf("peer", "gateway"), "gateway")
        core.nextOpen = outcome(update = session(), action = intent())
        bridge.onFrame("frame")

        assertEquals(listOf("host.start", "core.enable", "core.share", "core.open"), calls.take(4))
        assertEquals("gateway", core.enableContext?.localGatewayDeviceId)
        assertEquals(2, core.enableContext?.trustedRoster?.size)
        assertEquals(1, effects.size)
    }

    @Test fun `owned Go typing seals Google contact frame and non-owned or absent seal does nothing`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")

        bridge.receive("{\"google_conversation_id\":\"owned\",\"state\":\"started\"}")
        core.context = TypingReceiveContext("conversation", false)
        bridge.receive("{\"google_conversation_id\":\"other\",\"state\":\"started\"}")
        core.context = TypingReceiveContext("conversation", true)
        core.sealed = null
        bridge.receive("{\"google_conversation_id\":\"none\",\"state\":\"started\"}")

        assertEquals(listOf("frame"), transport.sent)
        assertEquals(NativeTypingState.STARTED, core.sealedState)
    }

    @Test fun `group Go typing carries its canonical participant into the seal and one-to-one stays participant-free`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")

        bridge.receive("{\"google_conversation_id\":\"group\",\"state\":\"started\",\"participant\":\"+12025550102\"}")
        assertEquals("+12025550102", core.sealedParticipant)
        assertEquals(listOf("frame"), transport.sent)

        bridge.receive("{\"google_conversation_id\":\"owned\",\"state\":\"stopped\"}")
        assertNull(core.sealedParticipant)
    }

    @Test fun `stale sequence expired deadline and repeated started never call Google`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "ok" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        core.nextOpen = outcome(update = session(), action = intent(sequence = 2UL, deadline = 9_999_999UL))
        bridge.onFrame("first")
        core.nextOpen = outcome(action = intent(sequence = 2UL, deadline = 9_999_999UL))
        bridge.onFrame("duplicate")
        core.nextOpen = outcome(action = intent(sequence = 3UL, deadline = 0UL))
        bridge.onFrame("expired")
        core.nextOpen = outcome(action = intent(sequence = 4UL, deadline = 9_999_999UL))
        bridge.onFrame("repeat")

        assertEquals(1, effects.size)
        assertEquals(9_999_999UL, effects.single().deadlineMono)
    }

    @Test fun `disable retires core and transport and no later frame reaches Google`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "ok" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        bridge.stop()
        core.nextOpen = outcome(update = session(), action = intent())
        bridge.onFrame("late")

        assertTrue(calls.contains("core.disable"))
        assertTrue(calls.contains("host.stop"))
        assertTrue(effects.isEmpty())
    }

    @Test fun `core reply frames are sent only by the current incarnation`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val executor = QueuedExecutor()
        val bridge = TypingBridge(
            ApplicationProvider.getApplicationContext(),
            transport,
            executor,
            { core },
            TypingSharingPreferences(ApplicationProvider.getApplicationContext()),
        )

        bridge.start(credentials())
        executor.runAll()
        transport.enable(listOf("gateway"), "gateway")
        executor.runAll()
        core.nextOpen = outcome(reply = "challenge")
        bridge.onFrame("frame")
        executor.runAll()
        assertEquals(listOf("challenge"), transport.sent)

        core.nextOpen = outcome(reply = "stale-ack")
        bridge.onFrame("frame")
        bridge.stop()
        executor.runAll()
        assertEquals(listOf("challenge"), transport.sent)
    }

    @Test fun `reply precedes follow ups and replay is recorded once`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        core.nextOpen = outcome(reply = "ack", followUps = listOf("first", "second"))
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.onFrame("challenge")
        }
        assertEquals(listOf("ack", "first", "second"), transport.sent)
        assertEquals(listOf(338, 325).map { "event=27 session=1 timestamp_ms=10 trace=0 29=$it" }, records)
    }

    @Test fun `follow ups stop after refused send including reply refusal`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val attempts = mutableListOf<String>()
        var refused = "second"
        val transport = object : TypingTransport {
            override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) = Unit
            override fun stop() = Unit
            override fun send(frameJson: String): Boolean { attempts += frameJson; return frameJson != refused }
        }
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
            { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext()))
        bridge.start(credentials())
        bridge.onEnabled(LiveTypingEnableContext(listOf("gateway"), "gateway"))
        core.nextOpen = outcome(reply = "ack", followUps = listOf("first", "second", "third"))
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.onFrame("challenge")
            refused = "ack"
            bridge.onFrame("challenge")
        }
        assertEquals(listOf("ack", "first", "second", "ack"), attempts)
        assertEquals(1, records.count { it.endsWith("29=338") })
        assertEquals(2, records.count { it.endsWith("29=321") })
    }

    @Test fun `retired authority sends neither reply nor follow ups`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val executor = QueuedExecutor()
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, executor,
            { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext()))
        bridge.start(credentials())
        executor.runAll()
        transport.enable(listOf("gateway"), "gateway")
        executor.runAll()
        core.nextOpen = outcome(reply = "ack", followUps = listOf("first"))
        bridge.onFrame("challenge")
        bridge.stop()
        executor.runAll()
        assertTrue(transport.sent.isEmpty())
    }

    @Test fun `outbound typing resolves the Google conversation id before the effect`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "ok" }

        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        core.nextOpen = outcome(update = session(), action = intent())
        bridge.onFrame("frame")

        assertEquals("google-conversation", effects.single().conversationId)
    }

    @Test fun `admitted typing without a link-owned Google id records outbound_not_owned`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls).also { it.ownedGoogleId = null }
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "ok" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")

        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            core.nextOpen = outcome(update = session(), action = intent())
            bridge.onFrame("frame")
        }

        assertTrue(effects.isEmpty())
        assertEquals(listOf("event=27 session=1 timestamp_ms=10 trace=0 29=280"), records)
    }

    @Test fun `failed Google typing records its reason and does not open the suppression window`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = bridge(core, transport, calls) { effects += it; "typing_failed" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")

        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            core.nextOpen = outcome(update = session(), action = intent())
            bridge.onFrame("first")
            core.nextOpen = outcome(action = intent(sequence = 2UL))
            bridge.onFrame("second")
        }

        assertEquals(2, effects.size)
        assertEquals(List(2) { "event=27 session=1 timestamp_ms=10 trace=0 29=331" }, records)
    }

    @Test fun `handshake is distinct from rejected intent`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            core.nextOpen = outcome(update = session())
            bridge.onFrame("handshake")
            core.nextOpen = outcome(action = intent(sequence = 1UL, deadline = 0UL))
            bridge.onFrame("expired")
        }
        assertEquals(listOf(
            "event=27 session=1 timestamp_ms=10 trace=0 29=325",
            "event=27 session=1 timestamp_ms=10 trace=0 29=59",
        ), records)
    }

    @Test fun `opened frame diagnostics distinguish ignored current stale and handshakes`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        val display = NativeLiveAction.Display("peer", 1UL, 1UL, "conversation", null, NativeTypingState.STARTED, 9999UL)
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            core.nextOpen = outcome()
            bridge.onFrame("empty")
            core.nextOpen = outcome(action = display)
            bridge.onFrame("display")
            core.nextOpen = NativeLiveOutcome(2UL, null, null, null, emptyList())
            bridge.onFrame("stale")
            core.nextOpen = outcome(reply = "reply")
            bridge.onFrame("reply")
            core.nextOpen = outcome(update = session())
            bridge.onFrame("session")
            core.nextOpen = outcome(action = intent(deadline = 0UL))
            bridge.onFrame("rejected")
        }
        assertEquals(listOf(336, 336, 337, 325, 325, 59).map {
            "event=27 session=1 timestamp_ms=10 trace=0 29=$it"
        }, records)
    }

    @Test fun `inbound malformed, unowned and refused seal have distinct reasons`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val bridge = bridge(core, transport, calls)
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.receive("not json")
            core.context = null
            bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
            core.context = TypingReceiveContext("conversation", true)
            core.sealed = null
            bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
        }
        assertEquals(listOf(316, 317, 318).map {
            "event=27 session=1 timestamp_ms=10 trace=0 29=$it"
        }, records)
    }

    @Test fun `Google typing refusal codes remain distinct and unknown uses fallback`() {
        val expected = listOf(
            "typing_unavailable" to 328, "typing_stale" to 329,
            "typing_expired" to 330, "typing_deadline" to 330,
            "typing_failed" to 331, "typing_cancelled" to 331,
            "typing_invalid" to 332, "typing_stopped" to 332,
            "typing_refused" to 333, "typing_unknown_conversation" to 333,
            "typing_not_one_to_one" to 333, "unknown" to 281,
        )
        expected.forEach { (result, reason) ->
            val calls = mutableListOf<String>()
            val core = FakeCore(calls)
            val bridge = bridge(core, FakeTransport(calls), calls) { result }
            bridge.start(credentials())
            bridge.onEnabled(LiveTypingEnableContext(listOf("gateway"), "gateway"))
            val records = mutableListOf<String>()
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
                core.nextOpen = outcome(update = session(), action = intent())
                bridge.onFrame("frame")
            }
            assertEquals(listOf("event=27 session=1 timestamp_ms=10 trace=0 29=$reason"), records)
        }
    }

    @Test fun `unavailable client records inbound open and enable reasons`() {
        val calls = mutableListOf<String>()
        val transport = FakeTransport(calls)
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
            { null }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
        bridge.start(credentials())
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.receive("{}")
            bridge.onFrame("{}")
            transport.enable(listOf("gateway"), "gateway")
        }
        listOf(315, 326, 322).forEach { reason ->
            assertTrue(records.any { it.endsWith("29=$reason") })
        }
    }

    @Test fun `seal and open exceptions and moved authority have distinct reasons`() {
        val calls = mutableListOf<String>()
        val delegate = FakeCore(calls)
        var sealFails = true
        var openFails = true
        val core = object : TypingCore by delegate {
            override fun seal(conversationId: String, gatewayDeviceId: String, participant: String?,
                state: NativeTypingState, clock: NativeLiveClock): NativeLiveOutbound? {
                if (sealFails) error("seal")
                return NativeLiveOutbound(2UL, "frame")
            }
            override fun open(frameJson: String, clock: NativeLiveClock): NativeLiveOutcome {
                if (openFails) error("open")
                return NativeLiveOutcome(2UL, null, null, "reply", emptyList())
            }
        }
        val transport = FakeTransport(calls)
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
            { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
            bridge.onFrame("frame")
            sealFails = false
            openFails = false
            bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
            bridge.onFrame("frame")
        }
        listOf(319, 320, 326, 327).forEach { reason ->
            assertTrue(records.any { it.endsWith("29=$reason") })
        }
    }

    @Test fun `enable and sharing exceptions have distinct reasons`() {
        listOf(false to 323, true to 324).forEach { (sharingFailure, reason) ->
            val calls = mutableListOf<String>()
            val core = object : TypingCore by FakeCore(calls) {
                override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong {
                    if (!sharingFailure) error("enable")
                    return 1UL
                }
                override fun setSharing(enabled: Boolean) { error("sharing") }
            }
            val transport = FakeTransport(calls)
            val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
                { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
            bridge.start(credentials())
            val records = mutableListOf<String>()
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
                transport.enable(listOf("gateway"), "gateway")
            }
            assertTrue(records.any { it.endsWith("29=$reason") })
        }
    }

    @Test fun `transport refusal records live send refusal not accepted frame`() {
        val core = FakeCore(mutableListOf())
        val transport = object : TypingTransport {
            override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) = Unit
            override fun stop() = Unit
            override fun send(frameJson: String) = false
        }
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
            { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
        bridge.start(credentials())
        bridge.onEnabled(LiveTypingEnableContext(listOf("gateway"), "gateway"))
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
        }
        assertTrue(records.any { it.endsWith("29=321") })
        assertFalse(records.any { it.endsWith("29=56") })
    }

    @Test fun `authority retired during seal records not current rather than send refusal`() {
        val calls = mutableListOf<String>()
        val delegate = FakeCore(calls)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val core = object : TypingCore by delegate {
            override fun seal(conversationId: String, gatewayDeviceId: String, participant: String?,
                state: NativeTypingState, clock: NativeLiveClock): NativeLiveOutbound? {
                entered.countDown()
                check(release.await(5, TimeUnit.SECONDS))
                return NativeLiveOutbound(1UL, "frame")
            }
        }
        val executor = Executors.newSingleThreadExecutor()
        val transport = FakeTransport(calls)
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, executor,
            { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
        try {
            bridge.start(credentials())
            executor.submit {}.get(5, TimeUnit.SECONDS)
            transport.enable(listOf("gateway"), "gateway")
            executor.submit {}.get(5, TimeUnit.SECONDS)
            val records = CopyOnWriteArrayList<String>()
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
                bridge.receive("""{"google_conversation_id":"id","state":"started"}""")
                assertTrue(entered.await(5, TimeUnit.SECONDS))
                bridge.stop()
                release.countDown()
                executor.submit {}.get(5, TimeUnit.SECONDS)
            }
            assertTrue(records.any { it.endsWith("29=320") })
            assertFalse(records.any { it.endsWith("29=321") })
            assertTrue(transport.sent.isEmpty())
        } finally {
            release.countDown()
            executor.shutdownNow()
        }
    }

    @Test fun `membership retirement drops an effect queued behind the bridge executor`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val executor = QueuedExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val bridge = TypingBridge(
            ApplicationProvider.getApplicationContext(),
            transport,
            executor,
            { core },
            TypingSharingPreferences(ApplicationProvider.getApplicationContext()),
        ) { effects += it; "ok" }

        bridge.start(credentials())
        executor.runAll()
        transport.enable(listOf("gateway"), "gateway")
        executor.runAll()
        core.nextOpen = outcome(update = session(), action = intent())
        bridge.onFrame("frame")
        executor.runNext()
        transport.disable()
        executor.runAll()

        assertTrue(effects.isEmpty())
    }

    @Test fun `retirement during the Google id lookup refuses the already admitted effect`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val transport = FakeTransport(calls)
        val executor = Executors.newSingleThreadExecutor()
        val lookupEntered = CountDownLatch(1)
        val releaseLookup = CountDownLatch(1)
        val blockingLookup = object : TypingCore by core {
            override fun ownedGoogleConversationId(conversationId: String): String? {
                lookupEntered.countDown()
                check(releaseLookup.await(5, TimeUnit.SECONDS))
                return "google-conversation"
            }
        }
        val setTypingCalls = mutableListOf<LiveTypingEffectIntent>()
        val bridge = TypingBridge(
            ApplicationProvider.getApplicationContext(),
            transport,
            executor,
            { blockingLookup },
            TypingSharingPreferences(ApplicationProvider.getApplicationContext()),
        ) { intent ->
            // LinkRuntime.setTyping refuses non-current intents before reaching the native handle.
            if (intent.isCurrent.isCurrent()) setTypingCalls += intent
            "typing_stale"
        }
        try {
            bridge.start(credentials())
            executor.submit {}.get(5, TimeUnit.SECONDS)
            transport.enable(listOf("gateway"), "gateway")
            executor.submit {}.get(5, TimeUnit.SECONDS)
            core.nextOpen = outcome(update = session(), action = intent())
            bridge.onFrame("frame")

            assertTrue(lookupEntered.await(5, TimeUnit.SECONDS))
            bridge.stop()
            releaseLookup.countDown()
            executor.submit {}.get(5, TimeUnit.SECONDS)

            assertTrue(setTypingCalls.isEmpty())
        } finally {
            releaseLookup.countDown()
            executor.shutdownNow()
        }
    }

    @Test fun `protocol fuse restarts lazily after cooldown and doubles the next cooldown`() {
        val factory = SocketFactory()
        val scheduler = ManualScheduler()
        val calls = mutableListOf<String>()
        val bridge = fuseBridge(factory, scheduler, calls)

        bridge.start(credentials())
        rejectCurrentSocket(factory); scheduler.runNext()
        rejectCurrentSocket(factory); scheduler.runNext()
        rejectCurrentSocket(factory)
        assertEquals(3, factory.sockets.size)
        assertEquals(0, scheduler.pendingCount())

        repeat(5) { bridge.start(credentials()) }

        assertEquals(3, factory.sockets.size)
        assertEquals(0, scheduler.pendingCount())

        ShadowSystemClock.advanceBy(Duration.ofMillis(299_999))
        bridge.start(credentials())
        assertEquals(3, factory.sockets.size)
        ShadowSystemClock.advanceBy(Duration.ofMillis(1))
        bridge.start(credentials())
        assertEquals(4, factory.sockets.size)

        repeat(3) { rejectCurrentSocket(factory); if (it < 2) scheduler.runNext() }
        ShadowSystemClock.advanceBy(Duration.ofMillis(599_999))
        bridge.start(credentials())
        assertEquals(6, factory.sockets.size)
        ShadowSystemClock.advanceBy(Duration.ofMillis(1))
        bridge.start(credentials())
        assertEquals(7, factory.sockets.size)
    }

    @Test fun `failed core enable retries on renewal after one minute`() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        var attempts = 0
        val flaky = object : TypingCore by core {
            override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong {
                attempts++
                if (attempts == 1) error("unavailable")
                return core.enable(context, clock)
            }
        }
        val transport = FakeTransport(calls)
        val bridge = TypingBridge(
            ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
            { flaky }, TypingSharingPreferences(ApplicationProvider.getApplicationContext()),
        ) { "ok" }
        bridge.start(credentials())
        transport.enable(listOf("gateway"), "gateway")
        ShadowSystemClock.advanceBy(Duration.ofMillis(59_999))
        transport.listener.onMembershipRenewed()
        assertEquals(1, attempts)
        ShadowSystemClock.advanceBy(Duration.ofMillis(1))
        transport.listener.onMembershipRenewed()
        assertEquals(2, attempts)
        assertTrue(calls.contains("core.share"))
        ShadowSystemClock.advanceBy(Duration.ofMinutes(1))
        transport.listener.onMembershipRenewed()
        assertEquals(2, attempts)
    }

    @Test fun `membership renewals from real host retry a failed enable once after one minute`() {
        val local = "00000000-0000-4000-8000-000000000000"
        val connection = "AAAAAAAAAAAAAAAAAAAAAA"
        val roster = """{"device_id":"$local","revoked":false,"key_epoch":1,"role":"gateway"}"""
        val factory = SocketFactory()
        val scheduler = ManualScheduler()
        val inline = Executor(Runnable::run)
        val host = LiveTypingHost.forTest(factory::create, scheduler,
            object : LiveClock { override fun elapsedRealtime() = android.os.SystemClock.elapsedRealtime() }, inline)
        val core = FakeCore(mutableListOf())
        var attempts = 0
        val flaky = object : TypingCore by core {
            override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong {
                attempts++
                if (attempts == 1) error("unavailable")
                return core.enable(context, clock)
            }
        }
        val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), HostTransport(host), inline,
            { flaky }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
        bridge.start(credentials().copy(localDeviceId = local))
        val socket = factory.sockets.single()
        val callback = factory.listeners.single()
        callback.onMessage(socket, """{"type":"membership_bootstrap","version":1,"connection_id":"$connection","membership_version":"1","roster":[$roster]}""")
        fun respond() {
            val challenge = org.json.JSONObject(socket.sent.last()).getString("challenge_id")
            callback.onMessage(socket, """{"type":"membership_response","version":1,"connection_id":"$connection","challenge_id":"$challenge","membership_version":"1","roster":[$roster]}""")
        }
        respond()
        assertEquals(1, attempts)
        repeat(3) {
            ShadowSystemClock.advanceBy(Duration.ofSeconds(20))
            scheduler.runNext()
            respond()
            assertEquals(if (it < 2) 1 else 2, attempts)
        }
        ShadowSystemClock.advanceBy(Duration.ofSeconds(20))
        scheduler.runNext()
        respond()
        assertEquals(2, attempts)
        bridge.stop()
    }

    @Test fun `disabled or stopped bridge clears pending enable`() {
        listOf(false, true).forEach { stop ->
            val calls = mutableListOf<String>()
            var attempts = 0
            val core = object : TypingCore by FakeCore(calls) {
                override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong {
                    attempts++
                    error("unavailable")
                }
            }
            val transport = FakeTransport(calls)
            val bridge = TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run),
                { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext())) { "ok" }
            bridge.start(credentials())
            transport.enable(listOf("gateway"), "gateway")
            if (stop) bridge.stop() else transport.disable()
            ShadowSystemClock.advanceBy(Duration.ofMinutes(1))
            transport.listener.onMembershipRenewed()
            assertEquals(1, attempts)
        }
    }

    @Test fun `authentication failure remains terminal until explicit stop`() {
        val calls = mutableListOf<String>()
        val transport = FakeTransport(calls)
        val bridge = bridge(FakeCore(calls), transport, calls)
        bridge.start(credentials())
        transport.listener.onDisabled(LiveTypingDisabledReason.AUTHENTICATION_FAILED)
        ShadowSystemClock.advanceBy(Duration.ofHours(2))
        bridge.start(credentials())
        assertEquals(1, calls.count { it == "host.start" })
        bridge.stop()
        bridge.start(credentials())
        assertEquals(2, calls.count { it == "host.start" })
    }

    @Test fun `membership revocation remains terminal until generation changes`() {
        val calls = mutableListOf<String>()
        val transport = FakeTransport(calls)
        val bridge = bridge(FakeCore(calls), transport, calls)
        bridge.start(credentials())
        transport.listener.onDisabled(LiveTypingDisabledReason.MEMBERSHIP_REVOKED)
        ShadowSystemClock.advanceBy(Duration.ofHours(2))
        bridge.start(credentials())
        assertEquals(1, calls.count { it == "host.start" })
        bridge.start(credentials(generation = 2))
        assertEquals(2, calls.count { it == "host.start" })
    }

    @Test fun `credential change restarts the host with the fuse reset`() {
        val factory = SocketFactory()
        val scheduler = ManualScheduler()
        val bridge = fuseBridge(factory, scheduler, mutableListOf())
        bridge.start(credentials())
        repeat(3) { rejectCurrentSocket(factory); if (it < 2) scheduler.runNext() }

        bridge.start(credentials(generation = 2))
        assertEquals(4, factory.sockets.size)

        rejectCurrentSocket(factory)
        assertEquals(1, scheduler.pendingCount())
        scheduler.runNext()
        assertEquals(5, factory.sockets.size)
    }

    @Test fun `explicit stop then start restarts the host with the fuse reset`() {
        val factory = SocketFactory()
        val scheduler = ManualScheduler()
        val bridge = fuseBridge(factory, scheduler, mutableListOf())
        bridge.start(credentials())
        repeat(3) { rejectCurrentSocket(factory); if (it < 2) scheduler.runNext() }

        bridge.stop()
        bridge.start(credentials())
        assertEquals(4, factory.sockets.size)

        rejectCurrentSocket(factory)
        assertEquals(1, scheduler.pendingCount())
    }

    @Test fun `stopped fused bridge does not restart on elapsed time`() {
        val factory = SocketFactory()
        val scheduler = ManualScheduler()
        val bridge = fuseBridge(factory, scheduler, mutableListOf())
        bridge.start(credentials())
        repeat(3) { rejectCurrentSocket(factory); if (it < 2) scheduler.runNext() }
        bridge.stop()
        ShadowSystemClock.advanceBy(Duration.ofHours(2))
        assertEquals(3, factory.sockets.size)
        assertEquals(0, scheduler.pendingCount())
    }

    private fun fuseBridge(factory: SocketFactory, scheduler: ManualScheduler, calls: MutableList<String>): TypingBridge {
        val inline = Executor(Runnable::run)
        val host = LiveTypingHost.forTest(factory::create, scheduler, executor = inline)
        return TypingBridge(
            ApplicationProvider.getApplicationContext(),
            HostTransport(host),
            inline,
            { FakeCore(calls) },
            TypingSharingPreferences(ApplicationProvider.getApplicationContext()),
        ) { "ok" }
    }

    private fun rejectCurrentSocket(factory: SocketFactory) {
        factory.listeners.last().onClosing(factory.sockets.last(), 1008, "invalid_live_frame")
    }

    private class HostTransport(private val host: LiveTypingHost) : TypingTransport {
        override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) = host.start(credentials, listener)
        override fun stop() = host.stop()
        override fun send(frameJson: String) = false
        override fun send(frameJson: String, publication: Long) = host.send(frameJson, publication)
    }

    private class SocketFactory {
        val sockets = CopyOnWriteArrayList<FakeSocket>()
        val listeners = CopyOnWriteArrayList<WebSocketListener>()
        fun create(request: Request, listener: WebSocketListener): WebSocket = FakeSocket().also {
            sockets += it
            listeners += listener
        }
    }

    private class FakeSocket : WebSocket {
        val sent = mutableListOf<String>()
        override fun request(): Request = Request.Builder().url("https://example.test").build()
        override fun queueSize() = 0L
        override fun send(text: String) = true.also { sent += text }
        override fun send(bytes: okio.ByteString) = false
        override fun close(code: Int, reason: String?) = true
        override fun cancel() {}
    }

    private class ManualScheduler : LiveScheduler {
        private class Entry(val delayMs: Long, val action: () -> Unit, var cancelled: Boolean = false)
        private val entries = CopyOnWriteArrayList<Entry>()
        override fun schedule(delayMs: Long, action: () -> Unit): LiveScheduledTask {
            val entry = Entry(delayMs, action)
            entries += entry
            return LiveScheduledTask { entry.cancelled = true }
        }
        fun pendingCount() = entries.count { !it.cancelled }
        fun runNext() {
            val entry = entries.first { !it.cancelled }
            entry.cancelled = true
            entry.action()
        }
    }

    private fun bridge(core: FakeCore, transport: FakeTransport, calls: MutableList<String>, effect: (LiveTypingEffectIntent) -> String = { "ok" }) =
        TypingBridge(ApplicationProvider.getApplicationContext(), transport, Executor(Runnable::run), { core }, TypingSharingPreferences(ApplicationProvider.getApplicationContext()), effect)

    private fun credentials(generation: Long = 1) = TypingCredentials("https://example.test", "bearer", "gateway", generation)
    private fun session() = NativeLiveSessionUpdate("peer", null, 1UL, 1UL)
    private fun intent(sequence: ULong = 1UL, deadline: ULong = 9_999_999UL) = NativeLiveAction.GatewayTypingIntent("peer", 1UL, sequence, "conversation", null, NativeTypingState.STARTED, deadline)
    private fun outcome(update: NativeLiveSessionUpdate? = null, action: NativeLiveAction? = null, reply: String? = null, followUps: List<String> = emptyList()) = NativeLiveOutcome(1UL, update, action, reply, followUps)

    private class FakeTransport(private val calls: MutableList<String>) : TypingTransport {
        lateinit var listener: LiveTypingListener
        val sent = mutableListOf<String>()
        override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) { calls += "host.start"; this.listener = listener }
        override fun stop() { calls += "host.stop" }
        override fun send(frameJson: String): Boolean { sent += frameJson; return true }
        fun enable(roster: List<String>, local: String) = listener.onEnabled(LiveTypingEnableContext(roster, local))
        fun disable() = listener.onDisabled(LiveTypingDisabledReason.MEMBERSHIP_REVOKED)
    }

    private class FakeCore(private val calls: MutableList<String>) : TypingCore {
        var enableContext: NativeLiveEnableContext? = null
        var context: TypingReceiveContext? = TypingReceiveContext("conversation", true)
        var sealed: NativeLiveOutbound? = NativeLiveOutbound(1UL, "frame")
        var sealedState: NativeTypingState? = null
        var sealedParticipant: String? = null
        var nextOpen = NativeLiveOutcome(1UL, null, null, null, emptyList())
        var ownedGoogleId: String? = "google-conversation"
        override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong { calls += "core.enable"; enableContext = context; return 1UL }
        override fun setSharing(enabled: Boolean) { calls += "core.share" }
        override fun disable() { calls += "core.disable" }
        override fun receiveContext(googleConversationId: String) = context
        override fun ownedGoogleConversationId(conversationId: String) = ownedGoogleId
        override fun seal(conversationId: String, gatewayDeviceId: String, participant: String?, state: NativeTypingState, clock: NativeLiveClock): NativeLiveOutbound? {
            sealedState = state
            sealedParticipant = participant
            return sealed
        }
        override fun open(frameJson: String, clock: NativeLiveClock): NativeLiveOutcome { calls += "core.open"; return nextOpen }
    }

    private class QueuedExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks += command }
        fun runNext() = tasks.removeFirst().run()
        fun runAll() { while (tasks.isNotEmpty()) runNext() }
    }
}
