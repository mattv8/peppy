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

        val prepared = adapter.prepare(COMMAND, 7, ROUTE, null)
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
            assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }
            assertThrows(IllegalStateException::class.java) { adapter.bindRoute("ticket", RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1) }
            assertThrows(IllegalStateException::class.java) { adapter.bindTarget("ticket", "google", "snapshot") }
            assertFalse(adapter.bindFacts("ticket", FACTS))
        }
    }

    @Test fun `prepare rejects wrong exact shape and revokes a known ticket`() {
        val ticket = "123e4567-e89b-12d3-a456-426614174000"
        val handle = FakeHandle().apply { prepareResponse = "{\"code\":\"prepared\",\"ticket_id\":\"$ticket\"}" }
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }

        assertEquals(listOf(ticket), handle.revoked)
    }

    @Test fun `prepare revokes a decoded ticket when later prepared metadata is invalid`() {
        val ticket = "123e4567-e89b-12d3-a456-426614174000"
        val handle = FakeHandle().apply {
            prepareResponse = "{\"code\":\"prepared\",\"ticket\\u005fid\":\"\\u0031${ticket.drop(1)}\",\"candidate_snapshot\":\"\"}"
        }
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }

        assertEquals(listOf(ticket), handle.revoked)
        assertEquals(0, handle.routeCalls)
        assertEquals("", handle.facts)
        assertEquals("", handle.commit)
    }

    @Test fun `does not revoke an id from a response rejected before decoding`() {
        val handle = FakeHandle().apply {
            prepareResponse = "{\"code\":\"prepared\",\"ticket_id\":\"123e4567-e89b-12d3-a456-426614174000\",\"candidate_snapshot\":\"\\q\"}"
        }
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }

        assertEquals(emptyList<String>(), handle.revoked)
    }

    @Test fun `rejects unsupported escapes while decoding valid escaped fields exactly`() {
        val malformed = "{\"code\":\"acc\\u+0065pted\"}"
        val handle = FakeHandle().apply {
            prepareResponse = malformed
            routeResponse = malformed
            targetResponse = malformed
            factsResponse = malformed
            commitResponse = malformed
        }
        val adapter = adapter(handle)

        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }
        assertThrows(IllegalStateException::class.java) { adapter.bindRoute("ticket", RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1) }
        assertThrows(IllegalStateException::class.java) { adapter.bindTarget("ticket", "google", "snapshot") }
        assertFalse(adapter.bindFacts("ticket", FACTS))
        assertEquals(RcsNativeCommitResult.OutcomeUnknown, adapter.commit("ticket", COMMIT))

        handle.prepareResponse = "{\"code\":\"pr\\u0065pared\",\"ticket\\u005fid\":\"123e4567-e89b-12d3-a456-426614174000\",\"candidate_snapshot\":\"\\uD83D\\uDE00\",\"run_incarnation\":\"run\",\"run_generation\":1,\"stream_epoch\":1,\"pairing_evidence_id\":\"pair\"}"
        handle.routeResponse = "{\"code\":\"route\\u005fbound\",\"observation\":\"\\uD83D\\uDE00\"}"
        val prepared = adapter.prepare(COMMAND, 1, ROUTE, null)

        assertEquals("😀", prepared.candidateSnapshotJson)
        assertEquals("😀", adapter.bindRoute(prepared.ticketId, RcsNativeContextTuple(1uL, 1uL, 1uL), 1uL, 1))
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
        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }
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
        assertEquals(candidateAtCap, adapter(FakeHandle().apply { prepareResponse = prepared(candidate = candidateAtCap) }).prepare(COMMAND, 1, ROUTE, null).candidateSnapshotJson)
        assertThrows(IllegalStateException::class.java) { adapter(FakeHandle().apply { prepareResponse = prepared(candidate = candidateOverCap) }).prepare(COMMAND, 1, ROUTE, null) }

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
        assertThrows(IllegalStateException::class.java) { adapter.prepare(COMMAND, 1, ROUTE, null) }
        assertEquals(RcsNativeCommitResult.Rejected, adapter.commit("ticket", COMMIT))
        assertEquals(0, handle.prepareCalls)
        assertEquals("", handle.commit)
    }

    @Test fun `guarded classification forwards the exact snapshot and captures its target`() {
        val handle = FakeHandle().apply { guarded = true; guardedSnapshot = " { exact candidate } " }

        val target = adapter(handle).classifyTarget("ticket") { snapshot ->
            assertEquals(handle.guardedSnapshot, snapshot)
            RcsNativeTarget.CarrierOnly
        }

        assertEquals(RcsNativeTarget.CarrierOnly, target)
        assertEquals("ticket", handle.guardedTicket)
    }

    @Test fun `guard rejection stale lease and callback failure fail closed without reentry`() {
        val rejected = FakeHandle().apply { guarded = false }
        assertEquals(null, adapter(rejected).classifyTarget("ticket") { RcsNativeTarget.CarrierOnly })
        assertEquals(1, rejected.guardCalls)

        val stale = FakeHandle()
        val lease = lease(stale)
        LinkRuntime.detachHandleForServiceStop(lease)?.let(LinkRuntime::closeDetachedHandle)
        assertEquals(null, RcsNativeGoAdapter(lease).classifyTarget("ticket") { RcsNativeTarget.CarrierOnly })
        assertEquals(0, stale.guardCalls)

        val throwing = FakeHandle()
        assertEquals(null, adapter(throwing).classifyTarget("ticket") { error("classifier failure") })
        assertEquals(1, throwing.guardCalls)
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

    @Test fun `prepare diagnostics distinguish exact refusals from malformed and bridge failures`() {
        val responses = listOf(
            "{\"code\":\"busy\"}" to 136L,
            "{\"code\":\"invalid_request\"}" to 137L,
            "{\"code\":\"rejected\"}" to 138L,
            "{\"code\":\"members_changed\"}" to 139L,
            "{\"code\":\"private-google-error\"}" to 141L,
            "{\"code\":\"rejected\",\"extra\":\"private-recipient\"}" to 141L,
        )
        responses.forEach { (response, reason) ->
            val handle = FakeHandle().apply { prepareResponse = response }
            val port = adapter(handle)
            val records = mutableListOf<String>()
            dev.peppy.mobile.diagnostics.SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
                assertThrows(RuntimeException::class.java) { port.prepare(COMMAND, 1, ROUTE, null) }
            }
            assertEquals(listOf("event=27 session=1 timestamp_ms=10 trace=0 29=$reason"), records)
            assertTrue(handle.revoked.isEmpty())
        }
        val port = adapter(FakeHandle().apply { throwPrepare = true })
        val records = mutableListOf<String>()
        dev.peppy.mobile.diagnostics.SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            assertThrows(IllegalStateException::class.java) { port.prepare(COMMAND, 1, ROUTE, null) }
        }
        assertEquals(listOf("event=27 session=1 timestamp_ms=10 trace=0 29=142"), records)
    }

    @Test fun `a decoded preparation records only its fixed success code`() {
        val port = adapter(FakeHandle())
        val records = mutableListOf<String>()
        dev.peppy.mobile.diagnostics.SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            assertEquals(CANDIDATE, port.prepare(COMMAND, 1, ROUTE, null).candidateSnapshotJson)
        }
        assertEquals(listOf("event=27 session=1 timestamp_ms=10 trace=0 29=140"), records)
    }

    @Test fun `carrier binding sources use the canonical Rust and Go wire enum names`() {
        val sources = listOf(
            RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber,
            RcsNativeOwnNumberSource.TelephonyLine1Number,
        )
        val actual = sources.map { source ->
            val handle = FakeHandle()
            adapter(handle).prepare(COMMAND, 1, ROUTE.copy(ownNumberSource = source), null)
            System.getenv("PEPPY_NATIVE_RCS_KOTLIN_FIXTURE_DIR")?.let { path ->
                val directory = java.io.File(path).also { it.mkdirs() }
                java.io.File(directory, "${source.name}.json").writeText(handle.prepareRequest)
            }
            org.json.JSONObject(handle.prepareRequest).getJSONObject("carrier_binding").getString("own_number_source")
        }
        assertEquals(listOf("SubscriptionManagerPhoneNumber", "TelephonyLine1Number"), actual)
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
        var throwPrepare = false
        var prepareRequest = ""
        var guarded = false; var guardCalls = 0; var guardedTicket = ""; var guardedSnapshot = CANDIDATE
        override fun pair(cookiesJson: ByteArray) = Unit; override fun relink() = Unit; override fun run() = Unit; override fun checkHealth() = 0L; override fun unlink() = Unit; override fun stop() = Unit; override fun close() = Unit; override fun status() = ""
        override fun ownerGeneration() = 1L
        override fun prepareNativeRcs(requestJson: String) = prepareResponse.also { prepareCalls++; prepareRequest = requestJson; if (throwPrepare) throw IllegalStateException("private-bridge-error") }
        override fun bindNativeRcsRoute(ticketId: String, linkGeneration: Long, sessionGeneration: Long, contextRevision: Long, mappingRevision: Long, observedAtMs: Long) = routeResponse.also { routeCalls++ }
        override fun withNativeRcsTargetClassification(ticketId: String, classifier: NativeRcsTargetClassifier): Boolean {
            guardCalls++; guardedTicket = ticketId
            return guarded && classifier.classify(guardedSnapshot)
        }
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
