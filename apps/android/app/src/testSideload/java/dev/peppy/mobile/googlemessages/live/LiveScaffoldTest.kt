package dev.peppy.mobile.googlemessages.live

import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class LiveScaffoldTest {
    private class Clock(@Volatile var now: Long = 0) : LiveClock {
        override fun elapsedRealtime() = now
    }

    private class Core(
        private val name: String = "",
        private val sharedEvents: MutableList<String>? = null,
        private val blockEnable: CountDownLatch? = null,
        private val releaseEnable: CountDownLatch? = null,
    ) : LiveCore {
        val events: MutableList<String> = Collections.synchronizedList(mutableListOf())
        val snapshots: MutableList<LiveMembershipSnapshot> = Collections.synchronizedList(mutableListOf())
        private val disabled = CountDownLatch(1)

        override fun enable(snapshot: LiveMembershipSnapshot) {
            events += "enable"
            sharedEvents?.add("enable$name")
            snapshots += snapshot
            blockEnable?.countDown()
            releaseEnable?.await(2, TimeUnit.SECONDS)
        }

        override fun disable() {
            events += "disable"
            sharedEvents?.add("disable$name")
            disabled.countDown()
        }

        fun awaitDisabled() = disabled.await(1, TimeUnit.SECONDS)
    }

    private class Cancellation(@Volatile override var cancelled: Boolean = false) : LiveCancellation

    private val id = "AAAAAAAAAAAAAAAAAAAAAA"
    private val otherId = "AQEBAQEBAQEBAQEBAQEBAQ"
    private val local = "00000000-0000-4000-8000-000000000000"
    private val peer = "00000000-0000-4000-8000-000000000001"
    private val roster = listOf(
        MembershipDevice(local, false, 1, MembershipRole.GATEWAY),
        MembershipDevice(peer, true, 0xffff_ffffL, MembershipRole.DEVICE),
    )

    private fun enabled(
        clock: Clock = Clock(),
        core: Core = Core(),
        coordinator: LiveIncarnationCoordinator = LiveIncarnationCoordinator(),
    ) = LiveConnection(clock, core, local, coordinator).also {
        assertTrue(it.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertTrue(it.issue(id))
        assertTrue(it.response(MembershipControl.Response(id, id, 1, roster)))
    }

    @Test
    fun `authoritative active roster enables once and unchanged renewal preserves publication`() {
        val clock = Clock()
        val core = Core()
        val connection = enabled(clock, core)
        assertEquals(1, core.events.count { it == "enable" })
        assertEquals(listOf(local), core.snapshots.single().activeRoster.map { it.deviceId })

        clock.now = 20_000
        assertTrue(connection.issue(otherId))
        assertTrue(connection.response(MembershipControl.Response(id, otherId, 1, roster)))
        assertEquals(1, core.events.count { it == "enable" })
    }

    @Test
    fun `empty missing revoked and duplicate local rosters never enable or renew`() {
        val invalidRosters = listOf(
            emptyList(),
            listOf(roster[1]),
            listOf(roster[0].copy(revoked = true)),
            listOf(roster[0], roster[0]),
        )
        for (invalid in invalidRosters) {
            val clock = Clock()
            val core = Core()
            val connection = LiveConnection(clock, core, local)
            assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
            assertTrue(connection.issue(id))
            assertFalse(connection.response(MembershipControl.Response(id, id, 1, invalid)))
            assertFalse(connection.enabled())
            assertTrue(core.events.isEmpty())
        }

        val clock = Clock()
        val core = Core()
        val connection = enabled(clock, core)
        clock.now = 20_000
        assertTrue(connection.issue(otherId))
        assertFalse(connection.response(MembershipControl.Response(id, otherId, 1, listOf(roster[1]))))
        assertFalse(connection.enabled())
        assertTrue(core.awaitDisabled())
        assertEquals(listOf("enable", "disable"), core.events)
    }

    @Test
    fun `old admission cannot publish into successor after hint disconnect or credential rotation`() {
        val clock = Clock()
        val coordinator = LiveIncarnationCoordinator()
        val oldConnection = enabled(clock, Core(), coordinator)
        val oldAdmission = oldConnection.admit(100_000)!!

        assertTrue(oldConnection.hint(MembershipControl.Hint(2)))
        clock.now = 20_000
        assertTrue(oldConnection.issue(otherId))
        assertTrue(oldConnection.response(MembershipControl.Response(id, otherId, 2, roster)))
        assertFalse(oldAdmission.publish(clock, 80_000) { fail("old incarnation action") })

        val successorAdmission = oldConnection.admit(100_000)!!
        oldConnection.retire()
        assertFalse(successorAdmission.publish(clock, 80_000) { fail("disconnected action") })

        val replacement = enabled(clock, Core(), coordinator)
        assertNotNull(replacement.admit(100_000))
        assertFalse(successorAdmission.publish(clock, 80_000) { fail("credential-rotated action") })
    }

    @Test
    fun `publication rechecks action and lease deadlines after runtime wait`() {
        val clock = Clock()
        val admission = enabled(clock).admit(10_000)!!
        assertFalse(admission.publish(clock, 60_000) { clock.now = 10_000 })

        val leaseClock = Clock()
        val leaseAdmission = enabled(leaseClock).admit(100_000)!!
        assertFalse(leaseAdmission.publish(leaseClock, 60_000) { leaseClock.now = 60_000 })
    }

    @Test
    fun `higher hint closes a blocked publication gate without waiting`() {
        val clock = Clock()
        val connection = enabled(clock)
        val actionEntered = CountDownLatch(1)
        val releaseAction = CountDownLatch(1)
        val worker = Thread {
            connection.admit(100_000)!!.publish(clock, 80_000) {
                actionEntered.countDown()
                releaseAction.await(2, TimeUnit.SECONDS)
            }
        }
        worker.start()
        try {
            assertTrue(actionEntered.await(1, TimeUnit.SECONDS))
            assertTrue(connection.hint(MembershipControl.Hint(2)))
            assertFalse(connection.enabled())
        } finally {
            releaseAction.countDown()
            worker.join(1_000)
        }
        assertFalse(worker.isAlive)
    }

    @Test
    fun `retire racing blocked enable disables and never enables publication`() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val clock = Clock()
        val core = Core(blockEnable = entered, releaseEnable = release)
        val connection = LiveConnection(clock, core, local)
        assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertTrue(connection.issue(id))
        val worker = Thread { connection.response(MembershipControl.Response(id, id, 1, roster)) }
        worker.start()
        try {
            assertTrue(entered.await(1, TimeUnit.SECONDS))
            connection.retire()
        } finally {
            release.countDown()
            worker.join(1_000)
        }
        assertFalse(worker.isAlive)
        assertFalse(connection.enabled())
        assertEquals(listOf("enable", "disable"), core.events)
    }

    @Test
    fun `shared coordinator disables old core before successor enable`() {
        val sharedEvents: MutableList<String> = Collections.synchronizedList(mutableListOf())
        val coordinator = LiveIncarnationCoordinator()
        val first = enabled(core = Core("1", sharedEvents), coordinator = coordinator)
        val second = enabled(core = Core("2", sharedEvents), coordinator = coordinator)

        assertEquals(listOf("enable1", "disable1", "enable2"), sharedEvents)
        assertFalse(first.enabled())
        assertTrue(second.enabled())
    }

    @Test
    fun `late pending response and expired old lease retire without a tick`() {
        val clock = Clock()
        val core = Core()
        val connection = enabled(clock, core)
        clock.now = 20_000
        assertTrue(connection.issue(otherId))
        clock.now = 25_001
        assertFalse(connection.response(MembershipControl.Response(id, otherId, 1, roster)))
        assertFalse(connection.admit(30_000)?.publish(clock, 80_000) { } ?: false)
        assertTrue(core.awaitDisabled())
        assertEquals(listOf("enable", "disable"), core.events)
    }

    @Test
    fun `response and lease deadline boundaries fail closed`() {
        val responseClock = Clock()
        val responseConnection = LiveConnection(responseClock, Core(), local)
        assertTrue(responseConnection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertTrue(responseConnection.issue(id))
        responseClock.now = 5_000
        assertTrue(responseConnection.response(MembershipControl.Response(id, id, 1, roster)))

        val leaseClock = Clock()
        val leaseConnection = enabled(leaseClock)
        leaseClock.now = 60_000
        assertNull(leaseConnection.admit(70_000))
        assertFalse(leaseConnection.enabled())

        val overflowClock = Clock(Long.MAX_VALUE - 1)
        val overflowConnection = LiveConnection(overflowClock, Core(), local)
        assertTrue(overflowConnection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertFalse(overflowConnection.issue(id))
        assertFalse(overflowConnection.enabled())
    }

    @Test
    fun `renewal after current lease expiry cannot resurrect`() {
        val clock = Clock()
        val core = Core()
        val connection = enabled(clock, core)
        clock.now = 59_000
        assertTrue(connection.issue(otherId))
        clock.now = 61_000
        assertFalse(connection.response(MembershipControl.Response(id, otherId, 1, roster)))
        assertFalse(connection.enabled())
    }

    @Test
    fun `higher response retires before a fresh challenge enables successor`() {
        val clock = Clock()
        val core = Core()
        val connection = enabled(clock, core)
        val oldAdmission = connection.admit(100_000)!!

        clock.now = 20_000
        assertTrue(connection.issue(otherId))
        assertTrue(connection.response(MembershipControl.Response(id, otherId, 2, roster)))
        assertFalse(connection.enabled())
        assertFalse(oldAdmission.publish(clock, 80_000) { fail("old version action") })
        assertEquals(listOf("enable", "disable"), core.events)

        clock.now = 40_000
        assertTrue(connection.issue(id))
        assertTrue(connection.response(MembershipControl.Response(id, id, 2, roster)))
        assertTrue(connection.enabled())
        assertEquals(listOf("enable", "disable", "enable"), core.events)
    }

    @Test
    fun `bootstrap response and hint state matrix rejects stale authority`() {
        val clock = Clock()
        val connection = enabled(clock)
        assertFalse(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertFalse(connection.bootstrap(MembershipControl.Bootstrap(otherId, 2, roster)))
        assertFalse(connection.response(MembershipControl.Response(id, id, 1, roster)))

        clock.now = 20_000
        assertTrue(connection.issue(otherId))
        assertTrue(connection.hint(MembershipControl.Hint(2)))
        assertFalse(connection.response(MembershipControl.Response(id, otherId, 1, roster)))
        assertFalse(connection.enabled())
        assertEquals(2, connection.highwater())
    }

    @Test
    fun `control budget is aggregate exact and data is separate drop new`() {
        val exact = enabled()
        assertTrue(exact.offerControl(MembershipControl.Hint(1), "x".repeat(32 * 1024)))

        val connection = enabled()
        repeat(64) { assertTrue(connection.offerData("x")) }
        assertFalse(connection.offerData("x"))
        assertTrue(connection.offerControl(MembershipControl.Hint(1), "x".repeat(16_384)))
        assertFalse(connection.offerControl(MembershipControl.Hint(1), "y".repeat(16_385)))
        assertFalse(connection.offerData("x"))
        assertFalse(connection.offerControl(MembershipControl.Hint(1), "x"))
    }

    @Test
    fun `wire parser accepts every fixed control and maximum u32`() {
        val device = "{\"device_id\":\"$local\",\"revoked\":false,\"key_epoch\":4294967295,\"role\":\"gateway\"}"
        assertTrue(MembershipControls.parse("{\"type\":\"membership_bootstrap\",\"version\":1,\"connection_id\":\"$id\",\"membership_version\":\"1\",\"roster\":[$device]}") is MembershipControl.Bootstrap)
        assertTrue(MembershipControls.parse("{\"type\":\"membership_response\",\"version\":1,\"connection_id\":\"$id\",\"challenge_id\":\"$otherId\",\"membership_version\":\"9223372036854775807\",\"roster\":[$device]}") is MembershipControl.Response)
        assertEquals(MembershipControl.Hint(2), MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"2\"}"))
    }

    @Test
    fun `wire parser rejects coercion unknown duplicate missing and noncanonical values`() {
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":\"1\",\"membership_version\":1}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"01\"}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"1\"}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1}"))
        assertNull(MembershipControls.parse("{\"type\":\"membership_hint\",\"version\":1,\"membership_version\":\"1\",\"extra\":false}"))
        val noncanonicalId = "AAAAAAAAAAAAAAAAAAAAAB"
        assertNull(MembershipControls.parse("{\"type\":\"membership_bootstrap\",\"version\":1,\"connection_id\":\"$noncanonicalId\",\"membership_version\":\"1\",\"roster\":[]}"))
        assertNull(MembershipControls.parse(" ".repeat(32 * 1024 + 1)))
    }

    @Test
    fun `handshake rejects late valid hello cancellation binary and closes exactly once`() {
        fun rejected(frame: LiveSocketFrame, afterReceive: () -> Unit = {}): Int {
            val clock = Clock()
            var closed = 0
            val cancellation = Cancellation()
            val socket = object : LiveSocket {
                override fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocketFrame {
                    afterReceive()
                    return frame
                }
                override fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) = Unit
                override fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation) { closed++ }
            }
            val lease = object : LiveLease {
                override fun open(deadlineElapsedMs: Long, cancellation: LiveCancellation) = socket
            }
            assertThrows(RuntimeException::class.java) { LiveTransport.open(lease, clock, cancellation) }
            return closed
        }

        assertEquals(1, rejected(LiveSocketFrame.Binary))
        val lateClock = Clock()
        var lateClosed = 0
        val lateSocket = object : LiveSocket {
            override fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocketFrame {
                lateClock.now = 5_000
                return LiveSocketFrame.Text("{\"type\":\"hello\",\"protocol_version\":2,\"max_ttl_ms\":15000,\"max_ciphertext_bytes\":5400}")
            }
            override fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) = Unit
            override fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation) { lateClosed++ }
        }
        val lateLease = object : LiveLease {
            override fun open(deadlineElapsedMs: Long, cancellation: LiveCancellation) = lateSocket
        }
        assertThrows(RuntimeException::class.java) { LiveTransport.open(lateLease, lateClock, Cancellation()) }
        assertEquals(1, lateClosed)
    }

    @Test
    fun `post handshake receive and write enforce frame deadlines cancellation and binary rejection`() {
        val clock = Clock()
        val cancellation = Cancellation()
        var sent = 0
        val socket = object : LiveSocket {
            var frame: LiveSocketFrame = LiveSocketFrame.Binary
            override fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation) = frame
            override fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) {
                sent++
                assertNotNull(text)
                (cancellation as Cancellation).cancelled = true
            }
            override fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation) = Unit
        }

        assertThrows(RuntimeException::class.java) { LiveTransport.receiveText(socket, clock, 1_000, cancellation) }
        socket.frame = LiveSocketFrame.Text("x".repeat(LiveTransport.MAX_TEXT_BYTES + 1))
        assertThrows(RuntimeException::class.java) { LiveTransport.receiveText(socket, clock, 1_000, cancellation) }
        socket.frame = LiveSocketFrame.Text("ok")
        assertEquals("ok", LiveTransport.receiveText(socket, clock, 1_000, cancellation))
        assertThrows(RuntimeException::class.java) { LiveTransport.write(socket, "ok", clock, 1_000, cancellation) }
        assertEquals(1, sent)
    }

    @Test
    fun `driver enforces cadence pending fresh nonce and enqueue failure retirement`() {
        val clock = Clock()
        val connection = LiveConnection(clock, Core(), local)
        assertTrue(connection.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertEquals(id, connection.drive({ id }) { true })
        assertNull(connection.drive({ otherId }) { true })
        assertTrue(connection.response(MembershipControl.Response(id, id, 1, roster)))

        clock.now = 19_999
        assertNull(connection.drive({ otherId }) { true })
        clock.now = 20_000
        assertEquals(otherId, connection.drive({ otherId }) { true })
        assertTrue(connection.response(MembershipControl.Response(id, otherId, 1, roster)))
        clock.now = 40_000
        assertNull(connection.drive({ otherId }) { true })

        val failed = LiveConnection(Clock(), Core(), local)
        assertTrue(failed.bootstrap(MembershipControl.Bootstrap(id, 1, roster)))
        assertNull(failed.drive({ id }) { false })
        assertFalse(failed.issue(otherId))
    }
}
