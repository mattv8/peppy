package dev.peppy.mobile.googlemessages.live

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class LiveScaffoldTest {
    private class Clock(var now: Long = 0) : LiveClock { override fun elapsedRealtime() = now }
    private class Core(private val blockEnable: CountDownLatch? = null, private val release: CountDownLatch? = null) : LiveCore {
        val events = mutableListOf<String>(); val snapshots = mutableListOf<LiveMembershipSnapshot>()
        override fun enable(snapshot: LiveMembershipSnapshot) { events += "enable"; snapshots += snapshot; blockEnable?.countDown(); release?.await(2, TimeUnit.SECONDS) }
        override fun disable() { events += "disable" }
    }
    private class Cancellation(override var cancelled: Boolean = false) : LiveCancellation
    private val id = "AAAAAAAAAAAAAAAAAAAAAA"
    private val local = "00000000-0000-4000-8000-000000000000"
    private val roster = listOf(MembershipDevice(local, false, 1, MembershipRole.GATEWAY), MembershipDevice("00000000-0000-4000-8000-000000000001", true, 0xffffffffL, MembershipRole.DEVICE))
    private fun enabled(clock: Clock = Clock(), core: Core = Core()) = LiveConnection(clock, core, local).also { assertTrue(it.bootstrap(MembershipControl.Bootstrap(id, 1, roster))); assertTrue(it.issue(id)); assertTrue(it.response(MembershipControl.Response(id, id, 1, roster))) }

    @Test fun `authoritative active roster enables once and unchanged renewal preserves publication`() {
        val clock = Clock(); val core = Core(); val connection = enabled(clock, core)
        assertEquals(1, core.events.count { it == "enable" }); assertEquals(listOf(local), core.snapshots.single().activeRoster.map { it.deviceId })
        clock.now = 20_000; assertTrue(connection.issue(id)); assertTrue(connection.response(MembershipControl.Response(id, id, 1, roster)))
        assertEquals(1, core.events.count { it == "enable" })
    }
    @Test fun `empty missing and revoked local rosters never enable`() {
        for (bad in listOf(emptyList(), listOf(roster[1]), listOf(roster[0].copy(revoked = true)))) {
            val clock = Clock(); val core = Core(); val connection = LiveConnection(clock, core, local)
            assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster))); assertTrue(connection.issue(id))
            assertFalse(connection.response(MembershipControl.Response(id, id, 1, bad))); assertFalse(connection.enabled()); assertTrue(core.events.isEmpty())
        }
    }
    @Test fun `old admission cannot publish into successor after hint`() {
        val clock = Clock(); val core = Core(); val connection = enabled(clock, core); val old = connection.admit(10_000)!!
        assertTrue(connection.hint(MembershipControl.Hint(2))); assertFalse(connection.enabled())
        assertFalse(old.publish(clock, 60_000) { fail("old incarnation action") })
    }
    @Test fun `retire racing blocked enable disables and never enables publication`() {
        val entered = CountDownLatch(1); val release = CountDownLatch(1); val clock = Clock(); val core = Core(entered, release); val connection = LiveConnection(clock, core, local)
        assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster))); assertTrue(connection.issue(id))
        val worker = Thread { connection.response(MembershipControl.Response(id, id, 1, roster)) }; worker.start()
        try { assertTrue(entered.await(1, TimeUnit.SECONDS)); connection.retire() } finally { release.countDown(); worker.join(1_000) }
        assertFalse(connection.enabled()); assertEquals(listOf("enable", "disable"), core.events)
    }
    @Test fun `late pending response and expired old lease retire without a tick`() {
        val clock = Clock(); val core = Core(); val connection = enabled(clock, core); clock.now = 20_000; assertTrue(connection.issue(id)); clock.now = 25_001
        assertFalse(connection.response(MembershipControl.Response(id, id, 1, roster))); assertFalse(connection.admit(30_000)?.publish(clock, 80_000) { } ?: false)
        val second = enabled(Clock(), Core()); // regression construction remains separately fresh
        assertNotNull(second)
    }
    @Test fun `renewal after current lease expiry cannot resurrect`() {
        val clock = Clock(); val core = Core(); val connection = enabled(clock, core); clock.now = 59_000; assertTrue(connection.issue(id)); clock.now = 61_000
        assertFalse(connection.response(MembershipControl.Response(id, id, 1, roster))); assertFalse(connection.enabled())
    }
    @Test fun `higher hint closes immediately and stale response cannot grant`() {
        val clock = Clock(); val core = Core(); val connection = enabled(clock, core); clock.now = 20_000; assertTrue(connection.issue(id)); assertTrue(connection.hint(MembershipControl.Hint(2)))
        assertFalse(connection.response(MembershipControl.Response(id, id, 1, roster))); assertFalse(connection.enabled()); assertEquals(2, connection.highwater())
    }
    @Test fun `control budget is aggregate and data is separate drop new`() {
        val connection = enabled(); repeat(64) { assertTrue(connection.offerData("x")) }; assertFalse(connection.offerData("x"))
        assertTrue(connection.offerControl(MembershipControl.Hint(1), "x".repeat(16_384))); assertFalse(connection.offerControl(MembershipControl.Hint(1), "y".repeat(16_385)))
        assertFalse(connection.offerData("x")); assertFalse(connection.offerControl(MembershipControl.Hint(1), "x"))
    }
    @Test fun `wire parser accepts fixed controls and rejects coercion`() {
        val device = "{\"device_id\":\"$local\",\"revoked\":false,\"key_epoch\":1,\"role\":\"gateway\"}"
        assertTrue(MembershipControls.parse("{\"type\":\"membership_bootstrap\",\"version\":1,\"connection_id\":\"$id\",\"membership_version\":\"1\",\"roster\":[$device]}") is MembershipControl.Bootstrap)
        assertTrue(MembershipControls.parse("{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$id\",\"challenge_id\":\"$id\",\"membership_version\":\"1\",\"roster\":[$device]}") is MembershipControl.Response)
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":\"1\",\"membership_version\":1}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"01\"}"))
        assertTrue(MembershipControls.bootstrap(id, 1, listOf(MembershipDevice(local, false, 0xffffffffL, MembershipRole.GATEWAY))))
    }
    @Test fun `handshake rejects late cancellation binary and closes`() {
        val clock = Clock(); var closed = 0; val cancellation = Cancellation(); val socket = object : LiveSocket {
            override fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocketFrame { clock.now = 6_000; cancellation as Cancellation; cancellation.cancelled = true; return LiveSocketFrame.Binary }
            override fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) = Unit
            override fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation) { closed++ }
        }
        val lease = object : LiveLease { override fun open(deadlineElapsedMs: Long, cancellation: LiveCancellation) = socket }
        assertThrows(RuntimeException::class.java) { LiveTransport.open(lease, clock, cancellation) }; assertEquals(1, closed)
    }
}
