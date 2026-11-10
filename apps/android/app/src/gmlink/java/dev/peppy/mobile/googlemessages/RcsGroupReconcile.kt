package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeGroupClassification
import uniffi.peppy_mobile_bindings.NativeGroupCreateBegin
import uniffi.peppy_mobile_bindings.NativeGroupCreateOutcome
import uniffi.peppy_mobile_bindings.gatewayBeginRcsGroupCreate
import uniffi.peppy_mobile_bindings.gatewayCommitRcsGroupCreate
import uniffi.peppy_mobile_bindings.gatewayIngestGroupCandidates
import uniffi.peppy_mobile_bindings.gatewaySettleRcsGroupCreate

/** Group reconciliation: one complete Go snapshot in, group links, pauses, and creates out. */
internal interface RcsGroupIngestPort {
    fun ingest(snapshotJson: String, nowMs: Long): NativeGroupClassification
}

/**
 * Feeds one complete conversation snapshot to core. Runs on link bootstrap, on index refresh, after a group create,
 * and once per unknown group sender or unmapped group record. Incomplete snapshots never reach core.
 */
internal class RcsGroupReconciler(
    private val port: RcsGroupIngestPort,
    private val clock: RcsBootstrapClock,
) {
    fun reconcile(snapshotJson: String): Boolean {
        val snapshot = runCatching { JSONObject(snapshotJson) }.getOrNull()
        if (snapshot == null || snapshot.optString("code") != "conversation_candidate_snapshot" || !snapshot.optBoolean("complete")) {
            recordGoogleMessagesLinkEvent("reconcile", "snapshot_unavailable", 0)
            return false
        }
        val ingested = runCatching { port.ingest(snapshotJson, clock.nowMs()) }.isSuccess
        if (!ingested) recordGoogleMessagesLinkEvent("reconcile", "ingest_failed", 0)
        return ingested
    }
}

/** Gateway-owned create lifecycle for one carrier-pending group text. The Go create is never retried here. */
internal interface RcsGroupCreateCore {
    fun begin(conversationId: String, commandId: String, nowMs: Long): NativeGroupCreateBegin
    fun commit(commandId: String, resultJson: String): NativeGroupCreateOutcome

    /** Persists unknown for a begun create whose answer did not commit. Never calls Google. */
    fun settle(commandId: String)
}

/**
 * The Go create request. An unnamed group omits `name`; Go treats an absent or empty name as unnamed.
 * Go's decoder pins this exact shape (group_create_test.go), so changing it needs both sides.
 */
internal fun groupCreateRequestJson(numbers: List<String>, name: String?): String =
    JSONObject().put("numbers", JSONArray(numbers)).apply { name?.let { put("name", it) } }.toString()

internal class RcsGroupCreateCoordinator(
    private val core: RcsGroupCreateCore,
    /** Runs `Link.createRcsGroup` under a captured link operation; null when the link is no longer current. */
    private val createRcsGroup: (String) -> String?,
    /**
     * Fresh snapshot ingest after the commit, so a created group links on the next complete index. Returns false when
     * the index could not be refreshed or ingested; the consumed create then stays unknown until the next pass.
     */
    private val reconcile: () -> Boolean,
    private val clock: RcsBootstrapClock,
) {
    /** Returns the outcome when a permit ran, or null when no create was needed or it was refused before Google. */
    fun create(conversationId: String, commandId: String): NativeGroupCreateOutcome? {
        val begin = runCatching { core.begin(conversationId, commandId, clock.nowMs()) }
        if (begin.isFailure) recordGoogleMessagesLinkEvent("create", "begin_failed", 0)
        val permit = begin.getOrNull() as? NativeGroupCreateBegin.Permit ?: return null
        val request = groupCreateRequestJson(permit.numbers, permit.name)
        // A throw, a missing reply, or a non-create reply all commit as unknown; core never retries an unknown create.
        val answer = runCatching { createRcsGroup(request) }.getOrNull()
        if (answer == null) recordGoogleMessagesLinkEvent("create", "link_unavailable", 0)
        val result = answer ?: UNKNOWN_RESULT
        val commit = runCatching { core.commit(commandId, result) }
        if (commit.isFailure) {
            recordGoogleMessagesLinkEvent("create", "commit_failed", 0)
            // Google already answered, so the begun create must not stay in flight: settle it as unknown.
            // A failed settle is retained by core and retried; it never repeats the Google call.
            if (runCatching { core.settle(commandId) }.isFailure) recordGoogleMessagesLinkEvent("create", "settle_failed", 0)
        }
        val outcome = commit.getOrDefault(NativeGroupCreateOutcome.UNKNOWN)
        when (outcome) {
            NativeGroupCreateOutcome.UNKNOWN -> recordGoogleMessagesLinkEvent("create", "unknown", 0)
            NativeGroupCreateOutcome.NOT_RCS -> recordGoogleMessagesLinkEvent("create", "not_rcs", 0)
            else -> Unit
        }
        if (!reconcile()) recordGoogleMessagesLinkEvent("reconcile", "after_create_failed", 0)
        return outcome
    }

    private companion object { const val UNKNOWN_RESULT = "{\"code\":\"unknown\"}" }
}

internal object RcsWallClock : RcsBootstrapClock {
    override fun nowMs() = System.currentTimeMillis()
}

internal class NativeRcsGroupIngestPort(private val client: NativeClient) : RcsGroupIngestPort {
    override fun ingest(snapshotJson: String, nowMs: Long) = gatewayIngestGroupCandidates(client, snapshotJson, nowMs)
}

internal class NativeRcsGroupCreateCore(private val client: NativeClient) : RcsGroupCreateCore {
    override fun begin(conversationId: String, commandId: String, nowMs: Long) = gatewayBeginRcsGroupCreate(client, conversationId, commandId, nowMs)
    override fun commit(commandId: String, resultJson: String) = gatewayCommitRcsGroupCreate(client, commandId, resultJson)
    override fun settle(commandId: String) = gatewaySettleRcsGroupCreate(client, commandId)
}
