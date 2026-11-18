package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeReactionAction
import uniffi.peppy_mobile_bindings.NativeReactionAttemptDecision
import uniffi.peppy_mobile_bindings.NativeReactionJob
import uniffi.peppy_mobile_bindings.NativeReactionOutcome
import uniffi.peppy_mobile_bindings.NativeReactionRefusal
import uniffi.peppy_mobile_bindings.NativeRcsReactionEntry

/** Phone-side reaction core operations; the production adapter wraps the generated `NativeClient`. */
internal interface RcsReactionCore {
    fun pendingJobs(): List<NativeReactionJob>
    fun begin(requestId: String): NativeReactionAttemptDecision
    fun recordOutcome(requestId: String, outcome: NativeReactionOutcome, reason: NativeReactionRefusal?)
}

/** Go reaction boundary. Each call returns the Go reply code; a thrown exception means no reply was read. */
internal interface RcsReactionGo {
    fun ownerGeneration(): Long
    fun isCurrent(): Boolean
    fun prepare(requestJson: String): String
    fun commit(requestJson: String): String
}

/**
 * Caller-thread-free, serialized coordinator for native reaction requests. Each job runs begin, then Go prepare,
 * then Go commit; core records the outcome. Only a commit that began can yield an unknown outcome.
 */
internal class RcsReactionCoordinator(
    private val go: RcsReactionGo,
    private val core: RcsReactionCore,
) {
    private val draining = AtomicBoolean(false)

    /** Processes every pending job once. A call while a drain runs returns at once; the running drain is the one that proceeds. */
    fun drain() {
        if (!draining.compareAndSet(false, true)) return
        try {
            val jobs = try {
                core.pendingJobs()
            } catch (failure: Exception) {
                preserveInterruption(failure)
                recordGoogleMessagesLinkEvent(KIND, "pending_failed", 0L)
                return
            }
            for (job in jobs) attempt(job)
        } finally {
            draining.set(false)
        }
    }

    private fun attempt(job: NativeReactionJob) {
        val decision = try {
            core.begin(job.requestId)
        } catch (failure: Exception) {
            preserveInterruption(failure)
            recordGoogleMessagesLinkEvent(KIND, "begin_failed", 0L)
            return
        }
        // A refused permit was already recorded as NotSent by core, so nothing more is written here.
        if (!decision.permitted) return recordGoogleMessagesLinkEvent(KIND, "refused", 0L)
        val requestId = decision.requestId ?: job.requestId
        val action = decision.action
        val googleConversationId = decision.googleConversationId
        val googleMessageId = decision.googleMessageId
        val emoji = decision.emoji
        if (action == null || googleConversationId == null || googleMessageId == null || emoji == null) {
            return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.REJECTED, "permit_incomplete")
        }
        if (action == NativeReactionAction.NOOP) return finish(requestId, NativeReactionOutcome.SENT, null, "noop")

        if (go.ownerGeneration() <= 0 || !go.isCurrent()) {
            return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.LINK_UNAVAILABLE, "owner_not_current")
        }
        val request = JSONObject()
            .put("request_id", requestId)
            .put("google_conversation_id", googleConversationId)
            .put("google_message_id", googleMessageId)
            .put("action", action.wireName())
            .put("emoji", emoji)
            .put("owner_generation", go.ownerGeneration())
            .toString()

        val prepared = try {
            go.prepare(request)
        } catch (failure: Exception) {
            preserveInterruption(failure)
            return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.REJECTED, "prepare_failed")
        }
        when (prepared) {
            "prepared" -> Unit
            "busy" -> return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.BUSY, "prepare_busy")
            "invalid_emoji" -> return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.INVALID_EMOJI, "prepare_invalid_emoji")
            else -> return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.REJECTED, "prepare_refused")
        }
        if (!go.isCurrent()) {
            return finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.LINK_UNAVAILABLE, "owner_changed")
        }

        // The commit call is the single dispatch point: any failure from here on is an unknown outcome.
        val committed = try {
            go.commit(request)
        } catch (failure: Exception) {
            preserveInterruption(failure)
            return finish(requestId, NativeReactionOutcome.UNKNOWN, null, "commit_failed")
        }
        when (committed) {
            "accepted" -> finish(requestId, NativeReactionOutcome.SENT, null, "accepted")
            "rejected" -> finish(requestId, NativeReactionOutcome.NOT_SENT, NativeReactionRefusal.REJECTED, "commit_rejected")
            else -> finish(requestId, NativeReactionOutcome.UNKNOWN, null, "outcome_unknown")
        }
    }

    private fun finish(requestId: String, outcome: NativeReactionOutcome, reason: NativeReactionRefusal?, code: String) {
        try {
            core.recordOutcome(requestId, outcome, reason)
        } catch (failure: Exception) {
            preserveInterruption(failure)
            recordGoogleMessagesLinkEvent(KIND, "record_failed", 0L)
            return
        }
        recordGoogleMessagesLinkEvent(KIND, code, 0L)
    }

    private fun NativeReactionAction.wireName(): String = when (this) {
        NativeReactionAction.ADD -> "add"
        NativeReactionAction.SWITCH -> "switch"
        NativeReactionAction.REMOVE -> "remove"
        NativeReactionAction.NOOP -> "noop"
    }

    private fun preserveInterruption(failure: Throwable) {
        if (failure is InterruptedException) Thread.currentThread().interrupt()
    }

    private companion object {
        const val KIND = "reaction"
    }
}

/** Production core port over the generated gateway client. Missing client means no attempt can be made. */
internal class RcsReactionCoreAdapter(private val client: () -> NativeClient?) : RcsReactionCore {
    constructor(context: android.content.Context) : this({ NativeGateway.open(context) })

    override fun pendingJobs(): List<NativeReactionJob> = client()?.pendingReactionJobs().orEmpty()

    override fun begin(requestId: String): NativeReactionAttemptDecision =
        client()?.beginReactionAttempt(requestId) ?: throw IllegalStateException("reaction_client_unavailable")

    override fun recordOutcome(requestId: String, outcome: NativeReactionOutcome, reason: NativeReactionRefusal?) {
        client()?.recordReactionOutcome(requestId, outcome, reason) ?: throw IllegalStateException("reaction_client_unavailable")
    }
}

/** Production Go port. Every call runs on the captured lease, or fails closed when that owner is no longer current. */
internal class RcsReactionGoAdapter(private val lease: LinkHandleLease) : RcsReactionGo {
    override fun ownerGeneration(): Long = LinkRuntime.withCapturedLinkOperation(lease) { it.ownerGeneration() } ?: 0L

    override fun isCurrent(): Boolean = LinkRuntime.ownsObservationLease(lease)

    override fun prepare(requestJson: String): String =
        replyCode(LinkRuntime.withCapturedLinkOperation(lease) { it.prepareReaction(requestJson) })

    override fun commit(requestJson: String): String =
        replyCode(LinkRuntime.withCapturedLinkOperation(lease) { it.commitPreparedReaction(requestJson) })

    /** A reply without a parseable `code` field is treated as no reply: prepare refuses, commit is unknown. */
    private fun replyCode(reply: String?): String = reply
        ?.let { runCatching { JSONObject(it).optString("code") }.getOrNull() }
        .orEmpty()
}

/** One reaction fact set from a projected receive record, ready for core ingestion. */
internal data class RcsReactionFacts(
    val googleConversationId: String,
    val googleMessageId: String,
    val reactions: List<NativeRcsReactionEntry>,
)

/**
 * Reads the record's `reactions` field. Absent means no reaction facts (null). A malformed field also yields null
 * and a fixed diagnostic; an empty array is an authoritative empty set.
 */
internal fun reactionFactsOf(record: String): RcsReactionFacts? {
    val json = runCatching { JSONObject(record) }.getOrNull() ?: return null
    if (!json.has("reactions")) return null
    val entries = runCatching { json.getJSONArray("reactions").toReactionEntries() }.getOrNull()
    if (entries == null) {
        recordGoogleMessagesLinkEvent("reaction", "ingest_invalid", 0L)
        return null
    }
    return RcsReactionFacts(
        googleConversationId = json.optString("google_conversation_id"),
        googleMessageId = json.optString("google_message_id"),
        reactions = entries,
    )
}

private fun JSONArray.toReactionEntries(): List<NativeRcsReactionEntry> = List(length()) { index ->
    val entry = getJSONObject(index)
    NativeRcsReactionEntry(
        emoji = entry.getString("emoji"),
        isSelf = entry.getBoolean("is_self"),
        addresses = entry.addressesOrEmpty(),
        unresolvedActors = entry.getInt("unresolved_actors").toUInt(),
    )
}

/** A missing or null `addresses` is an empty list, so format drift degrades the actor list instead of dropping the set. */
private fun JSONObject.addressesOrEmpty(): List<String> {
    val addresses = optJSONArray("addresses") ?: return emptyList()
    return List(addresses.length()) { addresses.getString(it) }
}
