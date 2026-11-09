package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.gateway.GatewayTestBase
import java.util.Collections
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeRcsContextExpectation
import uniffi.peppy_mobile_bindings.NativeRcsIngestOutcome
import uniffi.peppy_mobile_bindings.NativeRcsObservationProvenance
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsReceiveCoordinatorTest {
    @Test fun `disabled gate performs no receive or gap effects`() {
        val core = FakeCore().apply { failOnUse = true }
        val coordinator = coordinator(core, enabled = false)
        coordinator.receive(liveRecord("google-a"))
        coordinator.resume(FakeHandle { _, _ -> error("Go must not be called") })
        assertEquals(0, core.calls.get())
    }

    @Test fun `live events for one conversation are serialized while different conversations overlap`() {
        val contexts = mapOf(
            "google-a" to receiveContext("core-a", "google-a"),
            "google-b" to receiveContext("core-b", "google-b"),
        )
        val activeByConversation = ConcurrentHashMap<String, AtomicInteger>()
        val maxByConversation = ConcurrentHashMap<String, AtomicInteger>()
        val bothConversationsEntered = CountDownLatch(2)
        val release = CountDownLatch(1)
        val core = FakeCore(contextByGoogle = contexts).apply {
            onIngest = { context, _, _ ->
                val active = activeByConversation.getOrPut(context.conversationId) { AtomicInteger() }.incrementAndGet()
                maxByConversation.getOrPut(context.conversationId) { AtomicInteger() }.updateAndGet { maxOf(it, active) }
                bothConversationsEntered.countDown()
                assertTrue(release.await(5, TimeUnit.SECONDS))
                activeByConversation.getValue(context.conversationId).decrementAndGet()
                accepted()
            }
        }
        val coordinator = coordinator(core)
        val firstA = thread { coordinator.receive(liveRecord("google-a")) }
        assertTrue(waitUntilEntered(activeByConversation, "core-a"))
        val secondA = thread { coordinator.receive(liveRecord("google-a")) }
        val firstB = thread { coordinator.receive(liveRecord("google-b")) }
        assertTrue("different conversations should overlap", bothConversationsEntered.await(5, TimeUnit.SECONDS))
        assertEquals(1, maxByConversation.getValue("core-a").get())
        release.countDown()
        listOf(firstA, secondA, firstB).forEach { it.join(5_000); assertFalse(it.isAlive) }
        assertEquals(1, maxByConversation.getValue("core-a").get())
        assertEquals(1, maxByConversation.getValue("core-b").get())
    }

    @Test fun `failed page record keeps core token and retry starts from that token`() {
        val context = receiveContext("core-a", "google-a", "core-token")
        val outcomes = ArrayDeque(listOf(accepted(), refused(), accepted(), accepted()))
        val core = FakeCore(contextByGoogle = mapOf("google-a" to context), sweepContexts = listOf(context)).apply {
            onIngest = { _, _, _ -> outcomes.removeFirst() }
        }
        val handle = FakeHandle { _, token -> page(records = 2, nextCursor = "next-token") }
        val coordinator = coordinator(core)
        coordinator.resume(handle)
        coordinator.resume(handle)
        assertEquals(listOf("core-token", "core-token"), handle.pageTokens)
        assertEquals(listOf("next-token"), core.checkpoints)
    }

    @Test fun `successful durable checkpoint reloads the core token before continuation`() {
        val initial = receiveContext("core-a", "google-a", "token-0")
        val afterCheckpoint = receiveContext("core-a", "google-a", "token-1")
        val core = FakeCore(sweepContexts = listOf(initial)).apply {
            onContext = { afterCheckpoint }
        }
        val handle = FakeHandle { _, token ->
            when (token) {
                "token-0" -> page(records = 0, nextCursor = "token-1", hasMore = true)
                "token-1" -> page(records = 0, nextCursor = "token-2")
                else -> error("unexpected token")
            }
        }
        coordinator(core).resume(handle)
        assertEquals(listOf("token-0", "token-1"), handle.pageTokens)
        assertEquals(listOf("token-1", "token-2"), core.checkpoints)
        assertEquals(listOf("google-a"), core.contextReads)
    }

    @Test fun `fixed checkpoint refusal prevents continuation`() {
        val context = receiveContext("core-a", "google-a", "core-token")
        val core = FakeCore(sweepContexts = listOf(context)).apply {
            checkpointOutcome = "rejected:ownership_changed"
        }
        val handle = FakeHandle { _, _ -> page(records = 1, nextCursor = "next", hasMore = true) }
        coordinator(core).resume(handle)
        assertEquals(1, handle.pageTokens.size)
        assertEquals(listOf("next"), core.checkpointAttempts)
        assertTrue(core.contextReads.isEmpty())
    }

    @Test fun `checkpoint failure prevents continuation`() {
        val context = receiveContext("core-a", "google-a", "core-token")
        val core = FakeCore(sweepContexts = listOf(context)).apply {
            checkpointFailure = IllegalStateException("synthetic")
        }
        val handle = FakeHandle { _, _ -> page(records = 1, nextCursor = "next", hasMore = true) }
        coordinator(core).resume(handle)
        assertEquals(1, handle.pageTokens.size)
        assertEquals(listOf("next"), core.checkpointAttempts)
        assertTrue(core.contextReads.isEmpty())
    }

    @Test fun `restart sweep resumes every link owned conversation from its core token`() {
        val first = receiveContext("core-a", "google-a", "token-a")
        val second = receiveContext("core-b", "google-b", "token-b")
        val core = FakeCore(sweepContexts = listOf(first, second))
        val handle = FakeHandle { googleId, _ -> page(records = 0, nextCursor = "next-$googleId") }
        coordinator(core).resume(handle)
        assertEquals(listOf("google-a" to "token-a", "google-b" to "token-b"), handle.requests)
        assertEquals(listOf("next-google-a", "next-google-b"), core.checkpoints)
    }

    @Test fun `live and gap ingestion preserve acquisition provenance`() {
        val context = receiveContext("core-a", "google-a")
        val core = FakeCore(contextByGoogle = mapOf("google-a" to context), sweepContexts = listOf(context))
        val coordinator = coordinator(core)
        coordinator.receive(liveRecord("google-a"))
        coordinator.resume(FakeHandle { _, _ -> page(records = 1, nextCursor = "next") })
        assertEquals(listOf(NativeRcsObservationProvenance.LIVE, NativeRcsObservationProvenance.HISTORICAL), core.provenances)
    }

    @Test fun `live Go envelope ingests only its inner record`() {
        val context = receiveContext("core-a", "google-a")
        val core = FakeCore(contextByGoogle = mapOf("google-a" to context))

        coordinator(core).receive(liveRecord("google-a"))

        assertEquals(1, core.ingestedRecords.size)
        assertEquals("google-a", JSONObject(core.ingestedRecords.single()).getString("google_conversation_id"))
        assertEquals(null, JSONObject(core.ingestedRecords.single()).opt("code"))
        assertEquals(listOf(NativeRcsObservationProvenance.LIVE), core.provenances)
    }

    @Test fun `live protocol switch invalidates its current context without ingestion`() {
        val context = receiveContext("core-a", "google-a")
        val core = FakeCore(contextByGoogle = mapOf("google-a" to context))

        coordinator(core).receive(JSONObject().put("code", "protocol_switch").put("google_conversation_id", "google-a").toString())

        assertEquals(listOf("google-a"), core.invalidations)
        assertTrue(core.ingestedRecords.isEmpty())
    }

    @Test fun `cancellation during a page prevents later records and checkpoint`() {
        val context = receiveContext("core-a", "google-a")
        val cancelled = AtomicBoolean()
        val core = FakeCore(sweepContexts = listOf(context)).apply {
            onIngest = { _, _, _ -> cancelled.set(true); accepted() }
        }
        coordinator(core).resume(FakeHandle { _, _ -> page(records = 2, nextCursor = "next") }, cancelled)
        assertEquals(1, core.ingestedRecords.size)
        assertTrue(core.checkpointAttempts.isEmpty())
    }

    @Test fun `malformed projected record refusal prevents checkpoint`() {
        val context = receiveContext("core-a", "google-a")
        val core = FakeCore(sweepContexts = listOf(context)).apply {
            onIngest = { _, _, _ -> refused("invalid_request") }
        }
        coordinator(core).resume(FakeHandle { _, _ -> page(recordValues = listOf(JSONObject())) })
        assertEquals(1, core.ingestedRecords.size)
        assertTrue(core.checkpointAttempts.isEmpty())
    }

    @Test fun `stale Google event expectation stops the page without retry or checkpoint`() {
        val context = receiveContext("core-a", "google-a")
        val core = FakeCore(sweepContexts = listOf(context)).apply {
            onInvalidate = { actual, _ ->
                assertSame(context.expectation, actual.expectation)
                throw IllegalStateException("stale expectation")
            }
        }
        val response = page(records = 0, signals = listOf("google-a", "google-a"), nextCursor = "next")
        coordinator(core).resume(FakeHandle { _, _ -> response })
        assertEquals(1, core.invalidations.size)
        assertTrue(core.checkpointAttempts.isEmpty())
    }

    private fun coordinator(core: RcsReceiveCore, enabled: Boolean = true) = RcsReceiveCoordinator(
        ApplicationProvider.getApplicationContext(),
        core,
        { enabled },
    )

    private fun thread(block: () -> Unit) = Thread(block).apply { start() }

    private fun waitUntilEntered(active: ConcurrentHashMap<String, AtomicInteger>, conversationId: String): Boolean {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (System.nanoTime() < deadline) {
            if ((active[conversationId]?.get() ?: 0) > 0) return true
            Thread.yield()
        }
        return false
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsReceiveCoordinatorGatewaySmokeTest : GatewayTestBase() {
    @Test fun `generated projected observation binding returns fixed ownership refusal on a real store`() {
        val client = enrollAndUnlock()
        val observation = JSONObject()
            .put("google_message_id", "synthetic-message")
            .put("google_conversation_id", "synthetic-conversation")
            .put("direction", "incoming")
            .put("body", "synthetic body")
            .put("sent_at_ms", 1_735_689_600_123L)
            .put("status", "")
            .put("rcs_reported", false)
            .toString()
        val outcome = client.gatewayIngestRcsProjectedObservation(
            UUID.randomUUID().toString(),
            1u,
            observation,
            NativeRcsObservationProvenance.LIVE,
        )
        assertEquals("rejected:conversation_unproven", outcome.recordOutcome)
        assertEquals(null, outcome.statusOutcome)
        assertEquals(null, outcome.cursor)
    }
}

private class FakeCore(
    private val contextByGoogle: Map<String, NativeRcsReceiveContext> = emptyMap(),
    private val sweepContexts: List<NativeRcsReceiveContext> = emptyList(),
) : RcsReceiveCore {
    val calls = AtomicInteger()
    var failOnUse = false
    var checkpointFailure: Throwable? = null
    var checkpointOutcome = "recorded"
    var onContext: (String) -> NativeRcsReceiveContext? = { contextByGoogle[it] }
    var onIngest: (NativeRcsReceiveContext, String, NativeRcsObservationProvenance) -> NativeRcsIngestOutcome = { _, _, _ -> accepted() }
    var onInvalidate: (NativeRcsReceiveContext, String) -> Unit = { _, _ -> }
    val contextReads = Collections.synchronizedList(mutableListOf<String>())
    val ingestedRecords = Collections.synchronizedList(mutableListOf<String>())
    val provenances = Collections.synchronizedList(mutableListOf<NativeRcsObservationProvenance>())
    val checkpointAttempts = Collections.synchronizedList(mutableListOf<String>())
    val checkpoints = Collections.synchronizedList(mutableListOf<String>())
    val invalidations = Collections.synchronizedList(mutableListOf<String>())

    override fun context(googleId: String): NativeRcsReceiveContext? = used {
        contextReads += googleId
        onContext(googleId)
    }

    override fun contexts(): List<NativeRcsReceiveContext> = used { sweepContexts }

    override fun ingest(
        context: NativeRcsReceiveContext,
        record: String,
        provenance: NativeRcsObservationProvenance,
    ): NativeRcsIngestOutcome = used {
        ingestedRecords += record
        provenances += provenance
        onIngest(context, record, provenance)
    }

    override fun checkpoint(context: NativeRcsReceiveContext, token: String): String = used {
        checkpointAttempts += token
        checkpointFailure?.let { throw it }
        if (!checkpointOutcome.startsWith("rejected:")) checkpoints += token
        checkpointOutcome
    }

    override fun invalidate(context: NativeRcsReceiveContext, googleId: String) = used {
        invalidations += googleId
        onInvalidate(context, googleId)
    }

    private fun <T> used(block: () -> T): T {
        calls.incrementAndGet()
        check(!failOnUse) { "core must not be used" }
        return block()
    }
}

private class FakeHandle(private val fetch: (String, String) -> String) : LinkHandle {
    val requests = Collections.synchronizedList(mutableListOf<Pair<String, String>>())
    val pageTokens: List<String> get() = requests.map { it.second }
    override fun fetchRcsGap(googleConversationId: String, pageToken: String): String {
        requests += googleConversationId to pageToken
        return fetch(googleConversationId, pageToken)
    }
    override fun pair(cookiesJson: ByteArray) = Unit
    override fun relink() = Unit
    override fun run() = Unit
    override fun checkHealth() = 0L
    override fun unlink() = Unit
    override fun stop() = Unit
    override fun close() = Unit
    override fun status() = "{}"
}

private fun receiveContext(conversationId: String, googleId: String, token: String? = null) = NativeRcsReceiveContext(
    conversationId,
    googleId,
    7u,
    11u,
    NativeRcsReceiveOwner.LINK,
    token,
    NativeRcsContextExpectation(7u, 13u, 17u, 19u),
)

private fun accepted() = NativeRcsIngestOutcome("inserted", "status_applied", null)
private fun refused(code: String = "identity_conflict") = NativeRcsIngestOutcome("rejected:$code", null, null)
private fun liveRecord(googleId: String) = JSONObject()
    .put("code", "ok")
    .put("record", JSONObject()
        .put("google_message_id", "message-1")
        .put("google_conversation_id", googleId)
        .put("direction", "incoming")
        .put("body", "synthetic body")
        .put("sent_at_ms", 1_735_689_600_123L)
        .put("status", "delivered")
        .put("tmp_id", "tmp-1")
        .put("rcs_reported", true))
    .toString()
private fun page(
    records: Int = 0,
    recordValues: List<JSONObject> = List(records) { JSONObject().put("google_message_id", "message-$it") },
    signals: List<String> = emptyList(),
    nextCursor: String = "next",
    hasMore: Boolean = false,
): String = JSONObject()
    .put("code", "ok")
    .put("records", JSONArray(recordValues))
    .put("signals", JSONArray(signals.map { JSONObject().put("google_conversation_id", it) }))
    .put("next_cursor", nextCursor)
    .put("has_more", hasMore)
    .toString()
