package dev.peppy.mobile.googlemessages.live

import org.junit.Assert.*
import org.junit.Test

class LiveScaffoldTest {
    private class Clock(var now: Long = 0) : LiveClock { override fun elapsedRealtime() = now }
    private class Core : LiveCore { var enables = 0; var disables = 0; override fun enable() { enables++ }; override fun disable() { disables++ } }
    private val id = "AAAAAAAAAAAAAAAAAAAAAA"
    private val roster = listOf(MembershipDevice("00000000-0000-4000-8000-000000000000", false, 1, MembershipRole.GATEWAY))
    private class Cancellation(override var cancelled: Boolean = false) : LiveCancellation
    private class Socket(private val incoming: LiveSocketFrame) : LiveSocket {
        var writes = 0
        override fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation) = incoming
        override fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) { writes++; if (cancellation.cancelled) throw LiveCancelled() }
        override fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation) = Unit
    }
    @Test fun `bootstrap challenge snapshot enables and renewal does not reenable`() {
        val clock = Clock(); val core = Core(); val connection = LiveConnection(clock, core)
        assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertTrue(connection.issue(id)); assertTrue(connection.response(MembershipControl.Response(id, id, 1, roster)))
        assertEquals(1, core.enables); clock.now = 20_000
        assertTrue(connection.issue(id)); assertTrue(connection.response(MembershipControl.Response(id, id, 1, roster)))
        assertEquals(1, core.enables)
    }
    @Test fun `late replay hint and control overflow fail closed`() {
        val clock = Clock(); val core = Core(); val connection = LiveConnection(clock, core)
        connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)); connection.issue(id); clock.now = 5_001
        assertFalse(connection.response(MembershipControl.Response(id, id, 1, roster)))
        assertTrue(connection.hint(MembershipControl.Hint(2))); assertEquals(2, connection.highwater())
        repeat(4) { assertTrue(connection.offerControl(MembershipControl.Hint(2), 1)) }; assertFalse(connection.offerControl(MembershipControl.Hint(2), 1))
    }
    @Test fun `frozen timer resume expires issue-time lease and data drops new`() {
        val clock = Clock(); val core = Core(); val connection = LiveConnection(clock, core)
        connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)); connection.issue(id); connection.response(MembershipControl.Response(id, id, 1, roster))
        repeat(64) { assertTrue(connection.offerData("x")) }; assertFalse(connection.offerData("x"))
        clock.now = 60_000; assertFalse(connection.tick()); assertEquals(1, core.disables)
    }
    @Test fun `hello and caps reject invalid input`() {
        assertNull(MembershipControls.parse("{" + "x".repeat(32 * 1024) + "}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"01\"}"))
    }
    @Test fun `server-first handshake is fixed and cancellation prevents handshake and write`() {
        val clock = Clock(); val cancellation = Cancellation(); val socket = Socket(LiveSocketFrame.Text("{\"type\":\"hello\",\"protocol_version\":2,\"max_ttl_ms\":15000,\"max_ciphertext_bytes\":5400}"))
        val lease = object : LiveLease { override fun open(request: LiveRequest, deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocket { assertEquals("/v1/live", request.path); return socket } }
        assertSame(socket, LiveTransport.open(lease, "opaque", clock, cancellation))
        cancellation.cancelled = true
        assertThrows(LiveCancelled::class.java) { LiveTransport.write(socket, "x", 1, cancellation) }
        assertEquals(0, socket.writes)
    }
}
