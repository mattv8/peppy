package dev.peppy.mobile.googlemessages

import java.io.File
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeReactionAction
import uniffi.peppy_mobile_bindings.NativeReactionAttemptDecision
import uniffi.peppy_mobile_bindings.NativeReactionJob
import uniffi.peppy_mobile_bindings.NativeReactionOutcome
import uniffi.peppy_mobile_bindings.NativeReactionRefusal
import uniffi.peppy_mobile_bindings.NativeRcsReactionEntry

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsReactionCoordinatorTest {
    @Test fun `accepted commit records sent after one prepare and one commit with the permit fields`() {
        val go = FakeReactionGo()
        val core = FakeReactionCore(permit(action = NativeReactionAction.ADD))
        RcsReactionCoordinator(go, core).drain()

        assertEquals(listOf(NativeReactionOutcome.SENT to null), core.outcomes)
        assertEquals(1, go.commits.size)
        val request = JSONObject(go.prepared.single())
        assertEquals("request-1", request.getString("request_id"))
        assertEquals("g-conversation", request.getString("google_conversation_id"))
        assertEquals("g-message", request.getString("google_message_id"))
        assertEquals("add", request.getString("action"))
        assertEquals("👍", request.getString("emoji"))
        assertEquals(7L, request.getLong("owner_generation"))
        assertEquals(go.prepared.single(), go.commits.single())
    }

    @Test fun `switch and remove send their wire action names`() {
        val go = FakeReactionGo(commitReply = "accepted")
        RcsReactionCoordinator(go, FakeReactionCore(permit(action = NativeReactionAction.SWITCH))).drain()
        RcsReactionCoordinator(go, FakeReactionCore(permit(action = NativeReactionAction.REMOVE))).drain()
        assertEquals(listOf("switch", "remove"), go.prepared.map { JSONObject(it).getString("action") })
    }

    @Test fun `prepare refusals record not sent with the closest refusal and never commit`() {
        val cases = mapOf(
            "busy" to NativeReactionRefusal.BUSY,
            "invalid_emoji" to NativeReactionRefusal.INVALID_EMOJI,
            "rejected" to NativeReactionRefusal.REJECTED,
            "invalid_request" to NativeReactionRefusal.REJECTED,
        )
        for ((code, refusal) in cases) {
            val go = FakeReactionGo(prepareReply = code)
            val core = FakeReactionCore(permit())
            RcsReactionCoordinator(go, core).drain()
            assertEquals(listOf(NativeReactionOutcome.NOT_SENT to refusal), core.outcomes)
            assertEquals(0, go.commits.size)
        }
    }

    @Test fun `prepare exception records not sent and never commits`() {
        val go = FakeReactionGo(prepareFailure = IllegalStateException("synthetic"))
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.NOT_SENT), core.outcomes.map { it.first })
        assertEquals(0, go.commits.size)
    }

    @Test fun `commit rejected before dispatch records not sent`() {
        val go = FakeReactionGo(commitReply = "rejected")
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.NOT_SENT to NativeReactionRefusal.REJECTED), core.outcomes)
    }

    @Test fun `commit outcome unknown records unknown and is never retried`() {
        val go = FakeReactionGo(commitReply = "outcome_unknown")
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.UNKNOWN to null), core.outcomes)
        assertEquals(1, go.commits.size)
    }

    @Test fun `commit exception after dispatch began records unknown`() {
        val go = FakeReactionGo(commitFailure = IllegalStateException("synthetic"))
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.UNKNOWN to null), core.outcomes)
        assertEquals(1, go.commitAttempts)
    }

    @Test fun `noop records sent without any Go call`() {
        val go = FakeReactionGo()
        val core = FakeReactionCore(permit(action = NativeReactionAction.NOOP))
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.SENT to null), core.outcomes)
        assertEquals(0, go.prepared.size)
    }

    @Test fun `refused permit is already recorded by core and touches nothing else`() {
        val go = FakeReactionGo()
        val core = FakeReactionCore(NativeReactionAttemptDecision(false, null, null, null, null, null, NativeReactionRefusal.EXPIRED))
        RcsReactionCoordinator(go, core).drain()
        assertEquals(emptyList<Pair<NativeReactionOutcome, NativeReactionRefusal?>>(), core.outcomes)
        assertEquals(0, go.prepared.size)
    }

    @Test fun `owner not current before prepare records not sent and never prepares`() {
        val go = FakeReactionGo(current = false)
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.NOT_SENT to NativeReactionRefusal.LINK_UNAVAILABLE), core.outcomes)
        assertEquals(0, go.prepared.size)
    }

    @Test fun `owner change between prepare and commit records not sent without commit`() {
        val go = FakeReactionGo(currentAfterPrepare = false)
        val core = FakeReactionCore(permit())
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf(NativeReactionOutcome.NOT_SENT to NativeReactionRefusal.LINK_UNAVAILABLE), core.outcomes)
        assertEquals(0, go.commits.size)
    }

    @Test fun `pending jobs are each attempted in order`() {
        val go = FakeReactionGo()
        val core = FakeReactionCore(permit(), jobs = listOf(job("request-1"), job("request-2")))
        RcsReactionCoordinator(go, core).drain()
        assertEquals(listOf("request-1", "request-2"), core.begun)
        assertEquals(2, go.commits.size)
    }

    @Test fun `receive mapping reads the projected reactions field and treats its absence as no facts`() {
        assertNull(reactionFactsOf(JSONObject().put("google_message_id", "m").toString()))

        val malformed = JSONObject(RECORD).apply {
            put("reactions", JSONArray().put(JSONObject().put("emoji", "👍")))
        }.toString()
        assertNull(reactionFactsOf(malformed))

        val empty = JSONObject(RECORD).put("reactions", JSONArray()).toString()
        assertEquals(RcsReactionFacts("g-conversation", "g-message", emptyList()), reactionFactsOf(empty))

        val mapped = JSONObject(RECORD).put("reactions", JSONArray()
            .put(JSONObject().put("emoji", "❤️").put("is_self", true).put("addresses", JSONArray().put("+15550001111")).put("unresolved_actors", 1)))
            .toString()
        assertEquals(
            listOf(NativeRcsReactionEntry("❤️", true, listOf("+15550001111"), 1u)),
            reactionFactsOf(mapped)?.reactions,
        )
    }

    @Test fun `golden projected records from the Go lane map every reaction fact through the receive mapping`() {
        val cases = JSONArray(goldenRecordsFile().readText())
        val expected = mapOf(
            "self_only" to listOf(NativeRcsReactionEntry("👍", true, emptyList(), 0u)),
            "unresolved_only" to listOf(NativeRcsReactionEntry("🙏", false, emptyList(), 1u)),
            "mixed" to listOf(NativeRcsReactionEntry("❤️", true, listOf("+15551234567"), 1u)),
            "custom_with_unicode" to listOf(NativeRcsReactionEntry("👨‍👩‍👧", false, listOf("+15557654321"), 0u)),
        )
        assertEquals(expected.keys, List(cases.length()) { cases.getJSONObject(it).getString("name") }.toSet())
        for (index in 0 until cases.length()) {
            val case = cases.getJSONObject(index)
            val name = case.getString("name")
            val record = JSONObject()
                .put("google_conversation_id", "g-conversation")
                .put("google_message_id", "g-message")
                .put("reactions", case.getJSONArray("reactions"))
                .toString()
            assertEquals(name, expected.getValue(name), reactionFactsOf(record)?.reactions)
        }
    }

    @Test fun `missing or null addresses degrade to an empty actor list instead of dropping the set`() {
        val record = JSONObject()
            .put("google_conversation_id", "g-conversation")
            .put("google_message_id", "g-message")
            .put("reactions", JSONArray()
                .put(JSONObject().put("emoji", "👍").put("is_self", true).put("unresolved_actors", 0))
                .put(JSONObject().put("emoji", "🙏").put("is_self", false).put("addresses", JSONObject.NULL).put("unresolved_actors", 1)))
            .toString()
        assertEquals(
            listOf(
                NativeRcsReactionEntry("👍", true, emptyList(), 0u),
                NativeRcsReactionEntry("🙏", false, emptyList(), 1u),
            ),
            reactionFactsOf(record)?.reactions,
        )
    }

    private fun goldenRecordsFile(): File {
        val root = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .first { File(it, GOLDEN_RECORDS_PATH).isFile }
        return File(root, GOLDEN_RECORDS_PATH)
    }

    private fun job(requestId: String) = NativeReactionJob(requestId, "message-1", "conversation-1", "👍")

    private fun permit(action: NativeReactionAction = NativeReactionAction.ADD) = NativeReactionAttemptDecision(
        permitted = true,
        requestId = "request-1",
        googleConversationId = "g-conversation",
        googleMessageId = "g-message",
        action = action,
        emoji = "👍",
        reason = null,
    )

    private companion object {
        const val GOLDEN_RECORDS_PATH = "apps/android/googlemessages/gmlink/testdata/reaction_records.json"
        const val RECORD = """{"google_message_id":"g-message","google_conversation_id":"g-conversation"}"""
    }
}

private class FakeReactionCore(
    private val decision: NativeReactionAttemptDecision,
    private val jobs: List<NativeReactionJob> = listOf(NativeReactionJob("request-1", "message-1", "conversation-1", "👍")),
) : RcsReactionCore {
    val begun = mutableListOf<String>()
    /** Each recorded outcome with its refusal; the core ignores a null refusal for non-NotSent outcomes. */
    val outcomes = mutableListOf<Pair<NativeReactionOutcome, NativeReactionRefusal?>>()

    override fun pendingJobs(): List<NativeReactionJob> = jobs
    override fun begin(requestId: String): NativeReactionAttemptDecision {
        begun += requestId
        return decision
    }
    override fun recordOutcome(requestId: String, outcome: NativeReactionOutcome, reason: NativeReactionRefusal?) {
        outcomes += outcome to reason
    }
}

private class FakeReactionGo(
    private val generation: Long = 7L,
    private val current: Boolean = true,
    private val currentAfterPrepare: Boolean = true,
    private val prepareReply: String = "prepared",
    private val prepareFailure: Throwable? = null,
    private val commitReply: String = "accepted",
    private val commitFailure: Throwable? = null,
) : RcsReactionGo {
    val prepared = mutableListOf<String>()
    val commits = mutableListOf<String>()
    var commitAttempts = 0
    private var prepareDone = false

    override fun ownerGeneration(): Long = generation
    override fun isCurrent(): Boolean = if (prepareDone) currentAfterPrepare else current
    override fun prepare(requestJson: String): String {
        prepared += requestJson
        prepareFailure?.let { throw it }
        prepareDone = true
        return prepareReply
    }
    override fun commit(requestJson: String): String {
        commitAttempts += 1
        commitFailure?.let { throw it }
        commits += requestJson
        return commitReply
    }
}
