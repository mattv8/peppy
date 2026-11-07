package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RcsNativeGoAdapterTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Before fun setUp() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = { MemoryStore }
    }

    @After fun tearDown() {
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkRuntime.resetForTest()
    }

    @Test fun `forwards exact inner strings and decodes every phase`() {
        val handle = FakeHandle().apply {
            prepareResponse = prepared(candidate = CANDIDATE, generation = "9007199254740993", epoch = "18446744073709551615")
            routeResponse = observation("route_bound", ROUTE_OBSERVATION)
            targetResponse = observation("target_bound", TARGET_OBSERVATION)
        }
        val adapter = adapter(handle)

        val prepared = adapter.prepare(COMMAND, 7, ROUTE)
        val route = adapter.bindRoute(prepared.ticketId, RcsNativeContextTuple(1uL, 2uL, 3uL), 4uL, 5)
        val target = adapter.bindTarget(prepared.ticketId, "google", "snapshot")

        assertEquals(9_007_199_254_740_993uL, prepared.runGeneration)
        assertEquals(ULong.MAX_VALUE, prepared.streamEpoch)
        assertEquals(ROUTE_OBSERVATION, route)
        assertEquals(TARGET_OBSERVATION, target)
        assertTrue(adapter.bindFacts(prepared.ticketId, FACTS))
        assertEquals(FACTS, handle.facts)
        assertEquals(RcsNativeCommitResult.Accepted, adapter.commit(prepared.ticketId, COMMIT))
        assertEquals(COMMIT, handle.commit)
    }

    @Test fun `rejects malformed outer response classes for every precommit phase`() {
        val malformed = listOf(
            "{}", "{\"code\":null}", "{\"code\":\"facts_bound\",\"code\":\"facts_bound\"}",
            "{\"code\":\"facts_bound\"} trailing", "{\"code\":1}", "{\"code\":\"unknown\"}",
        )
        malformed.forEach { response ->
            val handle = FakeHandle().apply { prepareResponse = response; routeResponse = response; targetResponse = response; factsResponse = response }
            val adapter = adapter(handle)
            assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE) }
            assertThrows(IllegalStateException::class.java) { adapter.bindRoute("ticket", RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1) }
            assertThrows(IllegalStateException::class.java) { adapter.bindTarget("ticket", "google", "snapshot") }
            assertFalse(adapter.bindFacts("ticket", FACTS))
        }
    }

    @Test fun `prepare rejects wrong exact shape and revokes a known ticket`() {
        val ticket = "123e4567-e89b-12d3-a456-426614174000"
        val handle = FakeHandle().apply { prepareResponse = "{\"code\":\"prepared\",\"ticket_id\":\"$ticket\"}" }
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE) }

        assertEquals(listOf(ticket), handle.revoked)
    }

    @Test fun `checked signed boundary refuses bind route before dispatch`() {
        val handle = FakeHandle()
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) {
            adapter.bindRoute("ticket", RcsNativeContextTuple(Long.MAX_VALUE.toULong() + 1uL, 1uL, 1uL), 1uL, 1)
        }

        assertEquals(0, handle.routeCalls)
    }

    @Test fun `wrong prepare phase still revokes its known ticket`() {
        val handle = FakeHandle().apply { prepareResponse = prepared().replace("\"prepared\"", "\"target_bound\"") }
        val adapter = adapter(handle)
        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE) }
        assertEquals(listOf("123e4567-e89b-12d3-a456-426614174000"), handle.revoked)
    }

    @Test fun `translated interruptions preserve the thread interrupt flag`() {
        for (phase in listOf("facts", "commit", "revoke")) {
            val handle = FakeHandle().apply { interruptedPhase = phase }
            val adapter = adapter(handle)
            try {
                when (phase) {
                    "facts" -> assertFalse(adapter.bindFacts("ticket", FACTS))
                    "commit" -> assertEquals(RcsNativeCommitResult.OutcomeUnknown, adapter.commit("ticket", COMMIT))
                    "revoke" -> assertThrows(IllegalStateException::class.java) { adapter.revoke("ticket") }
                }
                assertTrue("$phase lost interruption", Thread.currentThread().isInterrupted)
            } finally {
                Thread.interrupted()
            }
        }
    }

    @Test fun `enforces inner candidate and observation caps without counting outer escapes`() {
        val candidateAtCap = "a".repeat(8 * 1024 * 1024)
        val candidateOverCap = candidateAtCap + "a"
        assertEquals(candidateAtCap, adapter(FakeHandle().apply { prepareResponse = prepared(candidate = candidateAtCap) }).prepare(COMMAND, 1, ROUTE).candidateSnapshotJson)
        assertThrows(IllegalStateException::class.java) { adapter(FakeHandle().apply { prepareResponse = prepared(candidate = candidateOverCap) }).prepare(COMMAND, 1, ROUTE) }

        val observationAtCap = "\\u003c".repeat(65_536 / 6) + "a".repeat(65_536 % 6)
        val observationOverCap = "a".repeat(65_537)
        assertEquals(observationAtCap, adapter(FakeHandle().apply { routeResponse = observation("route_bound", observationAtCap) }).bindRoute("ticket", RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1))
        assertThrows(IllegalStateException::class.java) { adapter(FakeHandle().apply { routeResponse = observation("route_bound", observationOverCap) }).bindRoute("ticket", RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1) }
    }

    @Test fun `commit malformed response call exception and unknown code are outcome unknown`() {
        val handle = FakeHandle()
        val adapter = adapter(handle)
        listOf("{}", "{\"code\":\"unknown\"}").forEach { response ->
            handle.commitResponse = response
            assertEquals(RcsNativeCommitResult.OutcomeUnknown, adapter.commit("ticket", COMMIT))
        }
        handle.throwCommit = true
        assertEquals(RcsNativeCommitResult.OutcomeUnknown, adapter.commit("ticket", COMMIT))
    }

    @Test fun `stale lease dispatches no phase`() {
        val handle = FakeHandle()
        val lease = lease(handle)
        LinkRuntime.detachHandleForServiceStop(lease)?.let(LinkRuntime::closeDetachedHandle)
        val adapter = RcsNativeGoAdapter(lease)

        assertEquals(0, adapter.ownerGeneration())
        assertFalse(adapter.isCurrent())
        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE) }
        assertEquals(RcsNativeCommitResult.Rejected, adapter.commit("ticket", COMMIT))
        assertEquals(0, handle.prepareCalls)
        assertEquals("", handle.commit)
    }

    @Test fun `revoke only accepts exact result codes`() {
        val handle = FakeHandle()
        val adapter = adapter(handle)
        handle.revokeResponse = "{\"code\":\"none\"}"
        assertEquals(RcsNativeRevokeResult.None, adapter.revoke("ticket"))
        listOf("{}", "{\"code\":\"unknown\"}", "{\"code\":null}").forEach { response ->
            handle.revokeResponse = response
            assertThrows(IllegalStateException::class.java) { adapter.revoke("ticket") }
        }
        handle.throwRevoke = true
        assertThrows(IllegalStateException::class.java) { adapter.revoke("ticket") }
    }

    @Test fun `direct revocation bypasses a held scoped operation lock`() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val handle = FakeHandle().apply {
            targetResponse = observation("target_bound", TARGET_OBSERVATION)
            onBindTarget = { entered.countDown(); assertTrue(release.await(1, TimeUnit.SECONDS)) }
        }
        val adapter = adapter(handle)
        val worker = thread { adapter.bindTarget("ticket", "google", "snapshot") }
        assertTrue(entered.await(1, TimeUnit.SECONDS))

        assertEquals(RcsNativeRevokeResult.Revoked, adapter.revoke("ticket"))
        release.countDown()
        worker.join(1_000)
        assertEquals(listOf("ticket"), handle.revoked)
    }

    @Test fun `old revoker never targets a replacement handle`() {
        val old = FakeHandle()
        val oldLease = lease(old)
        val oldAdapter = RcsNativeGoAdapter(oldLease)
        val detached = LinkRuntime.detachHandleForServiceStop(oldLease)
        LinkRuntime.closeDetachedHandle(requireNotNull(detached))
        val replacement = FakeHandle()
        lease(replacement)

        assertEquals(RcsNativeRevokeResult.Revoked, oldAdapter.revoke("old-ticket"))
        assertEquals(listOf("old-ticket"), old.revoked)
        assertTrue(replacement.revoked.isEmpty())
    }

    private fun adapter(handle: FakeHandle): RcsNativeGoAdapter {
        LinkRuntime.resetForTest()
        return RcsNativeGoAdapter(lease(handle))
    }

    private fun lease(handle: FakeHandle): LinkHandleLease {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        return requireNotNull(LinkRuntime.handleLease(context))
    }

    private class FakeHandle : LinkHandle {
        var prepareResponse = prepared(); var routeResponse = observation("route_bound", ROUTE_OBSERVATION); var targetResponse = observation("target_bound", TARGET_OBSERVATION)
        var factsResponse = "{\"code\":\"facts_bound\"}"; var commitResponse = "{\"code\":\"accepted\"}"; var revokeResponse = "{\"code\":\"revoked\"}"; var throwCommit = false; var throwRevoke = false
        var prepareCalls = 0; var routeCalls = 0; var facts = ""; var commit = ""; val revoked = mutableListOf<String>(); var onBindTarget: (() -> Unit)? = null
        var interruptedPhase: String? = null
        override fun pair(cookiesJson: ByteArray) = Unit; override fun relink() = Unit; override fun run() = Unit; override fun checkHealth() = 0L; override fun unlink() = Unit; override fun stop() = Unit; override fun close() = Unit; override fun status() = ""
        override fun ownerGeneration() = 1L
        override fun prepareNativeRcs(requestJson: String) = prepareResponse.also { prepareCalls++ }
        override fun bindNativeRcsRoute(ticketId: String, linkGeneration: Long, sessionGeneration: Long, contextRevision: Long, mappingRevision: Long, observedAtMs: Long) = routeResponse.also { routeCalls++ }
        override fun bindNativeRcsTarget(ticketId: String, googleConversationId: String, snapshotToken: String) = targetResponse.also { onBindTarget?.invoke() }
        override fun bindNativeRcsFacts(ticketId: String, factsJson: String) = factsResponse.also { interruptIf("facts"); facts = factsJson }
        override fun commitNativeRcs(ticketId: String, commitJson: String): String { interruptIf("commit"); if (throwCommit) throw IllegalStateException(); commit = commitJson; return commitResponse }
        override fun revokeNativeSend(ticketId: String): String { interruptIf("revoke"); if (throwRevoke) throw IllegalStateException(); revoked += ticketId; return revokeResponse }
        private fun interruptIf(phase: String) { if (interruptedPhase == phase) throw InterruptedException("synthetic interruption") }
    }

    private object MemoryStore : LinkStateStorage { override fun read() = ByteArray(0); override fun write(state: ByteArray) = Unit; override fun wipe() = Unit }

    private companion object {
        val COMMAND = RcsNativeCommand("123e4567-e89b-12d3-a456-426614174001", "conversation", "recipient")
        val ROUTE = RcsNativeRouteEvidence("route", 1, "+15550000", RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber, "US", 1)
        const val CANDIDATE = "{\"candidate\":\"\\uD83D\\uDE00\"}"
        const val ROUTE_OBSERVATION = "{\"route\":\"\\u003c&\\u003e\"}"
        const val TARGET_OBSERVATION = "{\"target\":\"\\uD83D\\uDE00\"}"
        const val FACTS = "{\"facts\":\"\\u003c&\\u003e\"}"
        const val COMMIT = "{\"commit\":\"\\uD83D\\uDE00\"}"
        fun prepared(candidate: String = CANDIDATE, generation: String = "1", epoch: String = "1") = "{\"code\":\"prepared\",\"ticket_id\":\"123e4567-e89b-12d3-a456-426614174000\",\"candidate_snapshot\":${quoted(candidate)},\"run_incarnation\":\"123e4567-e89b-12d3-a456-426614174002\",\"run_generation\":$generation,\"stream_epoch\":$epoch,\"pairing_evidence_id\":\"123e4567-e89b-12d3-a456-426614174003\"}"
        fun observation(code: String, value: String) = "{\"code\":\"$code\",\"observation\":${quoted(value)}}"
        fun quoted(value: String) = "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
    }
}
