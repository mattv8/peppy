package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.googlemessages.live.LiveScheduledTask
import dev.peppy.mobile.googlemessages.live.LiveScheduler
import dev.peppy.mobile.googlemessages.live.LiveTypingCredentials
import dev.peppy.mobile.googlemessages.live.LiveTypingDisabledReason
import dev.peppy.mobile.googlemessages.live.LiveTypingEnableContext
import dev.peppy.mobile.googlemessages.live.LiveTypingHost
import dev.peppy.mobile.googlemessages.live.LiveTypingListener
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
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
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
        assertTrue(effects.single().deadlineMono - effects.single().clock.nowMs() <= 5_000UL || effects.single().deadlineMono > 5_000UL)
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

    @Test fun `protocol rejections trip the host fuse and ordinary Google events never restart it`() {
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
        override fun request(): Request = Request.Builder().url("https://example.test").build()
        override fun queueSize() = 0L
        override fun send(text: String) = true
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
    private fun intent(sequence: ULong = 1UL, deadline: ULong = 9_999_999UL) = NativeLiveAction.GatewayTypingIntent("peer", 1UL, sequence, "conversation", NativeTypingState.STARTED, deadline)
    private fun outcome(update: NativeLiveSessionUpdate? = null, action: NativeLiveAction? = null, reply: String? = null) = NativeLiveOutcome(1UL, update, action, reply)

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
        var nextOpen = NativeLiveOutcome(1UL, null, null, null)
        override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong { calls += "core.enable"; enableContext = context; return 1UL }
        override fun setSharing(enabled: Boolean) { calls += "core.share" }
        override fun disable() { calls += "core.disable" }
        override fun receiveContext(googleConversationId: String) = context
        override fun ownedGoogleConversationId(conversationId: String) = "google-conversation"
        override fun seal(conversationId: String, gatewayDeviceId: String, state: NativeTypingState, clock: NativeLiveClock): NativeLiveOutbound? { sealedState = state; return sealed }
        override fun open(frameJson: String, clock: NativeLiveClock): NativeLiveOutcome { calls += "core.open"; return nextOpen }
    }

    private class QueuedExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks += command }
        fun runNext() = tasks.removeFirst().run()
        fun runAll() { while (tasks.isNotEmpty()) runNext() }
    }
}
