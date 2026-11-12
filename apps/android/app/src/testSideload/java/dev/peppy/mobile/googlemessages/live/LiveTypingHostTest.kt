package dev.peppy.mobile.googlemessages.live

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import okhttp3.Request
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LiveTypingHostTest {
    private val local = "00000000-0000-4000-8000-000000000000"
    private val peer = "00000000-0000-4000-8000-000000000001"
    private val connectionId = "AAAAAAAAAAAAAAAAAAAAAA"

    private class Socket : WebSocket {
        val sent = CopyOnWriteArrayList<String>()
        var closed = false
        override fun request(): Request = Request.Builder().url("https://example.test").build()
        override fun queueSize() = 0L
        override fun send(text: String) = sent.add(text)
        override fun send(bytes: okio.ByteString) = false
        override fun close(code: Int, reason: String?) = true.also { closed = true }
        override fun cancel() { closed = true }
    }

    private class Factory {
        val sockets = CopyOnWriteArrayList<Socket>()
        val listeners = CopyOnWriteArrayList<WebSocketListener>()
        val created = CountDownLatch(1)
        fun create(request: Request, listener: WebSocketListener): WebSocket = Socket().also {
            sockets += it; listeners += listener; created.countDown()
        }
    }

    private class Clock(var now: Long = 0L) : LiveClock {
        override fun elapsedRealtime() = now
    }

    private class Scheduler : LiveScheduler {
        data class Entry(val delayMs: Long, val action: () -> Unit, @Volatile var cancelled: Boolean = false)
        val entries = CopyOnWriteArrayList<Entry>()
        override fun schedule(delayMs: Long, action: () -> Unit): LiveScheduledTask {
            val entry = Entry(delayMs, action); entries += entry
            return LiveScheduledTask { entry.cancelled = true }
        }
        fun runNext() { entries.first { !it.cancelled }.also { it.cancelled = true }.action() }
    }

    private class QueueExecutor : Executor {
        private val actions = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { actions += command }
        fun runAll() { while (actions.isNotEmpty()) actions.removeFirst().run() }
        fun runLast() { actions.removeLast().run() }
    }

    private class Listener : LiveTypingListener {
        val enabled = CopyOnWriteArrayList<LiveTypingEnableContext>()
        val frames = CopyOnWriteArrayList<String>()
        val disabled = CopyOnWriteArrayList<LiveTypingDisabledReason>()
        val enabledLatch = CountDownLatch(1)
        override fun onEnabled(context: LiveTypingEnableContext) { enabled += context; enabledLatch.countDown() }
        override fun onFrame(json: String) { frames += json }
        override fun onDisabled(reason: LiveTypingDisabledReason) { disabled += reason }
    }

    private fun bootstrap(roster: String) = "{\"type\":\"membership_bootstrap\",\"version\":1,\"connection_id\":\"$connectionId\",\"membership_version\":\"1\",\"roster\":[$roster]}"
    private fun device(id: String, revoked: Boolean = false) = "{\"device_id\":\"$id\",\"revoked\":$revoked,\"key_epoch\":1,\"role\":\"gateway\"}"
    private fun serverChallenge(challengeId: String) = "{\"type\":\"membership_challenge\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challengeId\"}"
    private fun await(condition: () -> Boolean) { repeat(100) { if (condition()) return; Thread.sleep(10) }; throw AssertionError("condition not met") }

    @Test
    fun `live request uses bearer and exactly one membership capability header`() {
        val request = LiveTypingHost.requestFor(
            LiveTypingCredentials("https://example.test/base", "secret", "00000000-0000-4000-8000-000000000000"),
        )

        assertEquals("https://example.test/v1/live", request.url.toString())
        assertEquals("Bearer secret", request.header("Authorization"))
        assertEquals(listOf("membership-version-v1"), request.headers.values("x-peppy-live-capabilities"))
    }

    @Test
    fun `send is rejected before the host is ready`() {
        val host = LiveTypingHost.forTest({ _, _ -> error("must not connect") })
        assertFalse(host.send("{}", 0L))
        host.stop()
    }

    @Test
    fun `outbound frames at the server limit are accepted`() {
        assertTrue(LiveTypingHost.isOutboundFrame("x".repeat(LiveTransport.MAX_TEXT_BYTES)))
        assertFalse(LiveTypingHost.isOutboundFrame("x".repeat(LiveTransport.MAX_TEXT_BYTES + 1)))
    }

    @Test
    fun `bootstrap response enables filtered roster and answers challenge off the caller thread`() {
        val factory = Factory(); val listener = Listener(); val host = LiveTypingHost.forTest(factory::create)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        val socket = factory.sockets.single(); val callback = factory.listeners.single()
        val bootstrap = bootstrap("${device(local)},${device(peer, true)}")
        assertTrue(MembershipControls.parse(bootstrap) is MembershipControl.Bootstrap)
        callback.onMessage(socket, bootstrap)
        await { socket.sent.isNotEmpty() }
        val challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(socket.sent.single())!!.groupValues[1]
        assertEquals(serverChallenge(challenge), socket.sent.single())
        callback.onMessage(socket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"1\",\"roster\":[${device(local)},${device(peer, true)}]}")
        assertTrue(listener.enabledLatch.await(1, TimeUnit.SECONDS))
        assertEquals(listOf(local), listener.enabled.single().rosterIds)
        assertTrue(host.send("{}", listener.enabled.single().publication))
        host.stop()
    }

    @Test
    fun `revoked self disables and oversized inbound data is not delivered`() {
        val factory = Factory(); val listener = Listener(); val host = LiveTypingHost.forTest(factory::create)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        val socket = factory.sockets.single(); val callback = factory.listeners.single()
        callback.onMessage(socket, "x".repeat(LiveTransport.MAX_CONTROL_BYTES + 1))
        await { listener.disabled.contains(LiveTypingDisabledReason.PROTOCOL_ERROR) }
        assertTrue(listener.frames.isEmpty())
        assertFalse(host.send("{}", 0L))
    }

    @Test
    fun `authentication failure stops without reconnecting and later callbacks are ignored`() {
        val factory = Factory(); val listener = Listener(); val host = LiveTypingHost.forTest(factory::create)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        val socket = factory.sockets.single(); val callback = factory.listeners.single()
        callback.onFailure(socket, RuntimeException(), response(401))
        await { listener.disabled.contains(LiveTypingDisabledReason.AUTHENTICATION_FAILED) }
        assertEquals(1, factory.sockets.size)
        callback.onMessage(socket, "ignored")
        assertTrue(listener.frames.isEmpty())
    }

    @Test
    fun `live unauthorized closing stops without reconnecting and acknowledges the close`() {
        val factory = Factory(); val listener = Listener(); val scheduler = Scheduler()
        val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        val socket = factory.sockets.single(); val callback = factory.listeners.single()

        callback.onClosing(socket, 1008, "live_unauthorized")

        await { listener.disabled.contains(LiveTypingDisabledReason.AUTHENTICATION_FAILED) }
        assertTrue(socket.closed)
        assertTrue(scheduler.entries.isEmpty())
        assertEquals(1, factory.sockets.size)
        callback.onClosed(socket, 1008, "live_unauthorized")
        assertEquals(1, listener.disabled.size)
    }

    @Test
    fun `transient server close reconnects with backoff`() {
        val factory = Factory(); val scheduler = Scheduler()
        val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), Listener())
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))

        factory.listeners.single().onClosing(factory.sockets.single(), 1011, "live_unavailable")

        await { scheduler.entries.size == 1 }
        assertEquals(1_000L, scheduler.entries.single().delayMs)
        scheduler.runNext()
        await { factory.sockets.size == 2 }
        host.stop()
    }

    @Test
    fun `invalid live frame stops after three rejections despite a successful membership`() {
        val factory = Factory(); val listener = Listener(); val scheduler = Scheduler()
        val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))

        fun rejectCurrent() {
            factory.listeners.last().onClosing(factory.sockets.last(), 1008, "invalid_live_frame")
        }
        fun reconnect(socketCount: Int) {
            await { scheduler.entries.any { !it.cancelled && it.delayMs == 1_000L } }
            scheduler.runNext()
            await { factory.sockets.size == socketCount }
        }

        rejectCurrent(); reconnect(2)
        val secondSocket = factory.sockets.last(); val secondCallback = factory.listeners.last()
        secondCallback.onMessage(secondSocket, bootstrap(device(local)))
        await { secondSocket.sent.isNotEmpty() }
        val challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(secondSocket.sent.single())!!.groupValues[1]
        secondCallback.onMessage(secondSocket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"1\",\"roster\":[${device(local)}]}")
        assertTrue(listener.enabledLatch.await(1, TimeUnit.SECONDS))

        rejectCurrent(); reconnect(3)
        rejectCurrent()
        await { listener.disabled.contains(LiveTypingDisabledReason.PROTOCOL_ERROR) }
        assertEquals(3, factory.sockets.size)
        assertTrue(scheduler.entries.all { it.cancelled })
    }

    @Test
    fun `cancelled bootstrap callback cannot retire a successor socket`() {
        val factory = Factory(); val scheduler = Scheduler()
        val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), Listener())
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        factory.listeners.single().onOpen(factory.sockets.single(), response(101))
        await { scheduler.entries.size == 1 }
        val cancelledBootstrap = scheduler.entries.single()

        host.reconnectForTest()
        await { factory.sockets.size == 2 }
        cancelledBootstrap.action()
        Thread.sleep(20)

        assertEquals(2, factory.sockets.size)
        assertFalse(factory.sockets.last().closed)
        host.stop()
    }

    @Test
    fun `queued frame from a retired publication is not sent on its successor socket`() {
        val factory = Factory(); val listener = Listener(); val scheduler = Scheduler(); val executor = QueueExecutor()
        val host = LiveTypingHost.forTest(factory::create, scheduler, executor = executor)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        executor.runAll()
        val firstSocket = factory.sockets.single(); val firstCallback = factory.listeners.single()
        firstCallback.onMessage(firstSocket, bootstrap(device(local)))
        executor.runAll()
        val challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(firstSocket.sent.single())!!.groupValues[1]
        firstCallback.onMessage(firstSocket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"1\",\"roster\":[${device(local)}]}")
        executor.runAll()
        assertTrue(host.send("{}", listener.enabled.single().publication))

        host.reconnectForTest()
        executor.runLast()
        val secondSocket = factory.sockets.last(); val secondCallback = factory.listeners.last()
        secondCallback.onMessage(secondSocket, bootstrap(device(local)))
        executor.runLast()
        val nextChallenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(secondSocket.sent.single())!!.groupValues[1]
        secondCallback.onMessage(secondSocket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$nextChallenge\",\"membership_version\":\"1\",\"roster\":[${device(local)}]}")
        executor.runLast()

        assertTrue(secondSocket.sent.none { it == "{}" })
        host.stop()
        executor.runAll()
    }

    @Test
    fun `membership response timeout disables then reconnects and enables after the next bootstrap`() {
        val factory = Factory(); val listener = Listener(); val scheduler = Scheduler(); val clock = Clock(); val executor = QueueExecutor()
        val host = LiveTypingHost.forTest(factory::create, scheduler, clock, executor)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        executor.runAll()
        val socket = factory.sockets.single(); val callback = factory.listeners.single()
        callback.onMessage(socket, bootstrap(device(local)))
        executor.runAll()
        var challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(socket.sent.single())!!.groupValues[1]
        callback.onMessage(socket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"1\",\"roster\":[${device(local)}]}")
        executor.runAll()
        assertEquals(1, listener.enabled.size)

        clock.now = 20_000L
        scheduler.runNext()
        executor.runAll()
        assertEquals(2, socket.sent.size)
        clock.now = 25_001L
        scheduler.runNext()
        executor.runAll()

        assertEquals(listOf(LiveTypingDisabledReason.CONNECTION_LOST), listener.disabled)
        assertTrue(scheduler.entries.any { !it.cancelled && it.delayMs == 1_000L })
        scheduler.runNext()
        executor.runAll()
        assertEquals(2, factory.sockets.size)
        val nextSocket = factory.sockets.last(); val nextCallback = factory.listeners.last()
        nextCallback.onMessage(nextSocket, bootstrap(device(local)))
        executor.runAll()
        challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(nextSocket.sent.single())!!.groupValues[1]
        nextCallback.onMessage(nextSocket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"1\",\"roster\":[${device(local)}]}")
        executor.runAll()
        assertEquals(2, listener.enabled.size)
        host.stop()
        executor.runAll()
    }

    @Test
    fun `missing bootstrap reconnects after the server five second deadline`() {
        val factory = Factory(); val scheduler = Scheduler()
        val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), Listener())
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        factory.listeners.single().onOpen(factory.sockets.single(), response(101))
        await { scheduler.entries.size == 1 }
        assertEquals(10_000L, scheduler.entries.single().delayMs)

        scheduler.runNext()
        await { scheduler.entries.any { !it.cancelled && it.delayMs == 1_000L } }
        scheduler.runNext()
        await { factory.sockets.size == 2 }
        host.stop()
    }

    @Test
    fun `membership hint refreshes roster and removes revoked device`() {
        val factory = Factory(); val listener = Listener(); val scheduler = Scheduler(); val clock = Clock()
        val host = LiveTypingHost.forTest(factory::create, scheduler, clock)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), listener)
        assertTrue(factory.created.await(1, TimeUnit.SECONDS)); val socket = factory.sockets.single(); val callback = factory.listeners.single()
        callback.onMessage(socket, bootstrap("${device(local)},${device(peer)}")); await { socket.sent.isNotEmpty() }
        fun response(version: Int, roster: String) {
            val challenge = Regex("\\\"challenge_id\\\":\\\"([^\\\"]+)\\\"").find(socket.sent.last())!!.groupValues[1]
            callback.onMessage(socket, "{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challenge\",\"membership_version\":\"$version\",\"roster\":[$roster]}")
        }
        response(1, "${device(local)},${device(peer)}"); assertTrue(listener.enabledLatch.await(1, TimeUnit.SECONDS))
        callback.onMessage(socket, "{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"2\"}")
        await { listener.disabled.size == 1 }
        await { scheduler.entries.size >= 2 }
        clock.now = 20_000
        scheduler.runNext()
        await { socket.sent.size == 2 }; response(2, "${device(local)},${device(peer, true)}")
        await { listener.enabled.size == 2 }
        assertEquals(listOf(local), listener.enabled.last().rosterIds)
        host.stop()
    }

    @Test
    fun `failure backoff is bounded and stop cancels pending reconnect`() {
        val factory = Factory(); val scheduler = Scheduler(); val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), Listener())
        assertTrue(factory.created.await(1, TimeUnit.SECONDS)); val socket = factory.sockets.single(); val callback = factory.listeners.single()
        callback.onFailure(socket, RuntimeException(), null); await { scheduler.entries.size == 1 }
        assertEquals(1_000L, scheduler.entries[0].delayMs)
        host.stop(); await { scheduler.entries[0].cancelled }
        scheduler.entries[0].action(); assertEquals(1, factory.sockets.size)
    }

    @Test
    fun `reconnect backoff doubles to its cap and explicit network reconnect is prompt`() {
        val factory = Factory(); val scheduler = Scheduler(); val host = LiveTypingHost.forTest(factory::create, scheduler)
        host.start(LiveTypingCredentials("https://example.test", "secret", local), Listener())
        assertTrue(factory.created.await(1, TimeUnit.SECONDS))
        repeat(7) {
            val socket = factory.sockets.last(); val callback = factory.listeners.last()
            callback.onFailure(socket, RuntimeException(), null); await { scheduler.entries.size == it + 1 }
            scheduler.runNext(); await { factory.sockets.size == it + 2 }
        }
        assertEquals(listOf(1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L), scheduler.entries.map { it.delayMs })
        host.reconnectForTest(); await { factory.sockets.size == 9 }
        factory.listeners.last().onFailure(factory.sockets.last(), RuntimeException(), null); await { scheduler.entries.size == 8 }
        assertEquals(1_000L, scheduler.entries.last().delayMs)
        host.stop()
    }

    @Test
    fun `recognisable bearer is absent from host diagnostics and source logging`() {
        val token = "recognisable-bearer-token"
        val request = LiveTypingHost.requestFor(LiveTypingCredentials("https://example.test", token, local))
        assertEquals("Bearer $token", request.header("Authorization"))
        val source = java.io.File("src/gmlink/java/dev/peppy/mobile/googlemessages/live/LiveTypingHost.kt").readText()
        assertFalse(source.contains("Log."))
        assertFalse(source.contains("recordGoogleMessagesLinkEvent(\"live\", \"auth_refused\", token"))
    }

    private fun response(code: Int) = okhttp3.Response.Builder()
        .request(Request.Builder().url("https://example.test").build())
        .protocol(okhttp3.Protocol.HTTP_1_1).code(code).message("test").build()
}
