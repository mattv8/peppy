package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeRcsIngestOutcome
import uniffi.peppy_mobile_bindings.NativeRcsObservationProvenance
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/** Result of one per-conversation recovery run. */
internal enum class RcsRecoveryResult {
    /** The watermark reached the run's target and the gap is closed. */
    COMPLETE,

    /** Progress is kept (cap hit) or a failed page cleared the hint. A later trigger retries. */
    INCOMPLETE,

    /** An ingest, signal or core write failed, or the run was cancelled. The last durable write stands. */
    ABORTED,
}

internal class RcsReceiveCoordinator(
    context: Context,
    private val core: RcsReceiveCore = GeneratedRcsReceiveCore(context.applicationContext),
    private val enabled: () -> Boolean = GoogleMessagesLinkService::nativeRcsEnabled,
    private val bootstrapRequest: (String) -> Unit = GoogleMessagesLinkService::requestTargetedRcsBootstrap,
    /** One fresh complete snapshot ingest for group records; false when no complete snapshot was available. */
    private val reconcileGroups: () -> Boolean = GoogleMessagesLinkService::requestRcsGroupReconcile,
    private val nowMs: () -> Long = System::currentTimeMillis,
    /** Requests a gateway pass after core invalidates a mapping for a protocol-switch event. Best effort. */
    private val requestUpload: () -> Unit = { GatewayWork.enqueueAfterCapture(context.applicationContext) },
) : RcsReceiveSink {
    private val locks = ConcurrentHashMap<String, Any>()
    private val sweepLock = Any()
    private var sweepContinuation: String? = null

    override fun receive(recordJson: String) {
        if (!enabled()) return
        val event = parseLiveEvent(recordJson) ?: return
        val googleId = event.googleConversationId
        var context = runCatching { core.context(googleId) }.getOrNull()
        if (context == null && event is LiveEvent.Record && reconcileGroups()) {
            // An unmapped group record: the fresh snapshot may have linked or created its group. Retry once.
            context = runCatching { core.context(googleId) }.getOrNull()
        }
        if (context == null) {
            if (event is LiveEvent.Record) bootstrapRequest(googleId)
            return
        }
        // Unknown group senders get one snapshot re-read per record before the record is ingested unattributed.
        if (event is LiveEvent.Record && unknownGroupSender(event.record)) reconcileGroups()
        var invalidated = false
        synchronized(lock(context.conversationId)) {
            if (!enabled()) return
            when (event) {
                is LiveEvent.Record -> runCatching {
                    val outcome = core.ingest(context, event.record, NativeRcsObservationProvenance.LIVE)
                    GoogleMessagesLinkService.requestRcsMediaReceive()
                    // The proof was withdrawn after the context was read. A new proof reopens the
                    // lane, and recovery then fetches this record from the unchanged watermark.
                    if (outcome.recordOutcome == UNPROVEN_REFUSAL) bootstrapRequest(googleId)
                }
                is LiveEvent.ProtocolSwitch -> {
                    invalidated = runCatching { core.invalidate(context, googleId, null) }.getOrNull() == true
                    if (invalidated) recordGoogleMessagesLinkEvent("recovery", "live_switch_invalidated", 0)
                }
            }
        }
        if (invalidated) requestGatewayUpload()
    }

    private fun requestGatewayUpload() {
        runCatching { requestUpload() }
    }

    private fun unknownGroupSender(record: String): Boolean =
        runCatching { JSONObject(record).optBoolean("sender_unknown") }.getOrDefault(false)

    /**
     * Restart and ready sweep. Skips conversations whose index shows no activity since their watermark, and
     * recovers at most [SWEEP_CAP] others. The next sweep continues after the last recovered conversation and wraps.
     */
    fun resume(
        handle: LinkHandle,
        cancelled: AtomicBoolean = AtomicBoolean(),
        current: () -> Boolean = { true },
    ) {
        if (!enabled() || !current()) return
        val contexts = runCatching { core.contexts() }.getOrDefault(emptyList())
        if (contexts.isEmpty()) return
        val activity = lastActivity(handle, contexts)
        val ordered = orderForSweep(contexts, activity)
        val start = sweepStart(ordered)
        var recovered = 0
        var skippedIdle = 0
        var last: String? = null
        for (context in ordered.drop(start) + ordered.take(start)) {
            if (recovered == SWEEP_CAP || !active(cancelled, current)) break
            if (isIdle(context, activity[context.googleConversationId])) {
                skippedIdle += 1
                continue
            }
            recover(handle, context, cancelled, current)
            recovered += 1
            last = context.googleConversationId
        }
        synchronized(sweepLock) { sweepContinuation = last }
        if (skippedIdle > 0) recordGoogleMessagesLinkEvent("link", "rcs_sweep_skipped_idle", 0)
    }

    /** Targeted recovery for one owned conversation, used after a claim and for owned targets. */
    fun resumeConversation(
        handle: LinkHandle,
        googleConversationId: String,
        cancelled: AtomicBoolean = AtomicBoolean(),
        current: () -> Boolean = { true },
    ): RcsRecoveryResult {
        val context = runCatching { core.context(googleConversationId) }.getOrNull()
            ?: return aborted("context_missing")
        return recover(handle, context, cancelled, current)
    }

    private fun recover(
        handle: LinkHandle,
        initial: NativeRcsReceiveContext,
        cancelled: AtomicBoolean,
        current: () -> Boolean,
    ): RcsRecoveryResult {
        val uploadNeeded = AtomicBoolean()
        val result = synchronized(lock(initial.conversationId)) { traverse(handle, initial, cancelled, current, uploadNeeded) }
        when (result) {
            RcsRecoveryResult.ABORTED -> recordGoogleMessagesLinkEvent("link", "rcs_recovery_aborted", 0)
            RcsRecoveryResult.INCOMPLETE -> recordGoogleMessagesLinkEvent("link", "rcs_recovery_incomplete", 0)
            RcsRecoveryResult.COMPLETE -> Unit
        }
        if (uploadNeeded.get()) requestGatewayUpload()
        return result
    }

    /**
     * Walks backward from the hint cursor, or from the newest page. Records in the slack window are ingested.
     * Records older than `watermark - SLACK` are never ingested. A page with only such records, or the last page,
     * completes the run. Progress is written after each page's ingests and before the next fetch.
     */
    private fun traverse(
        handle: LinkHandle,
        initial: NativeRcsReceiveContext,
        cancelled: AtomicBoolean,
        current: () -> Boolean,
        uploadNeeded: AtomicBoolean,
    ): RcsRecoveryResult {
        var context = initial
        val floorMs = initial.watermarkMs - SLACK_MS
        val hint = initial.resumeHint?.takeIf { it.targetMs >= initial.watermarkMs }
        var cursor: String? = hint?.cursor
        var targetMs: Long? = hint?.targetMs
        val usedCursors = HashSet<String>()
        // One snapshot re-read per conversation traversal, before its first unattributed group record.
        var rereadDone = false
        repeat(MAX_PAGES) {
            if (!active(cancelled, current)) return aborted("cancelled")
            val requested = cursor.orEmpty()
            usedCursors += requested
            val page = fetchPage(handle, context.googleConversationId, requested) ?: return failedPage(context)
            val pageTarget = targetMs ?: page.records.maxOfOrNull { it.sentAtMs }
            targetMs = pageTarget
            for (record in page.records) {
                if (!active(cancelled, current)) return aborted("cancelled")
                if (record.sentAtMs >= floorMs && !rereadDone && unknownGroupSender(record.json)) {
                    reconcileGroups()
                    rereadDone = true
                }
                if (record.sentAtMs >= floorMs && ingestAborts(context, record.json)) return aborted("ingest_refused")
            }
            for (signal in page.signals) {
                if (!active(cancelled, current)) return aborted("cancelled")
                val invalidated = runCatching { core.invalidate(context, signal.googleConversationId, signal.sentAtMs) }
                    .getOrElse { return aborted("signal_failed") }
                if (invalidated) {
                    uploadNeeded.set(true)
                    recordGoogleMessagesLinkEvent("recovery_signal_age", signalAge(signal.sentAtMs), 0)
                    return aborted("signal_invalidated")
                }
                recordGoogleMessagesLinkEvent("recovery_skipped_age", signalAge(signal.sentAtMs), 0)
                recordGoogleMessagesLinkEvent("recovery", "stale_signal_skipped", 0)
            }
            if (!page.hasMore || (page.records.isNotEmpty() && page.records.all { it.sentAtMs < floorMs })) {
                return finish(context, pageTarget)
            }
            val next = page.nextCursor
            if (next == null || next in usedCursors) return failedPage(context)
            if (pageTarget != null) {
                if (!write { core.recordProgress(context, pageTarget, next) }) return aborted("progress_failed")
                context = runCatching { core.context(context.googleConversationId) }.getOrNull()
                    ?: return aborted("context_reload_missing")
            }
            cursor = next
        }
        return RcsRecoveryResult.INCOMPLETE
    }

    /** Closes the gap at [targetMs]. An empty first page writes nothing unless a stale gap or hint must be cleared. */
    private fun finish(context: NativeRcsReceiveContext, targetMs: Long?): RcsRecoveryResult {
        if (targetMs == null && !context.incomplete && context.resumeHint == null) return RcsRecoveryResult.COMPLETE
        return if (write { core.complete(context, targetMs ?: context.watermarkMs) }) RcsRecoveryResult.COMPLETE else aborted("finish_failed")
    }

    /** A fetch, cursor or sanity failure: keep the watermark, drop the hint, and leave the gap open. */
    private fun failedPage(context: NativeRcsReceiveContext): RcsRecoveryResult =
        if (write { core.clearHint(context) }) RcsRecoveryResult.INCOMPLETE else aborted("clear_hint_failed")

    private fun aborted(code: String): RcsRecoveryResult {
        recordGoogleMessagesLinkEvent("recovery_abort", code, 0)
        return RcsRecoveryResult.ABORTED
    }

    private fun signalAge(sentAtMs: Long?): String {
        if (sentAtMs == null) return "absent"
        val ageMs = nowMs() - sentAtMs
        return when {
            ageMs < 0 -> "future"
            ageMs < TWO_MINUTES_MS -> "under_2m"
            ageMs < 15 * 60 * 1000L -> "under_15m"
            ageMs < 60 * 60 * 1000L -> "under_1h"
            ageMs < 24 * 60 * 60 * 1000L -> "under_24h"
            else -> "older"
        }
    }

    private fun fetchPage(handle: LinkHandle, googleId: String, cursor: String): Page? {
        val response = runCatching { handle.fetchRcsGap(googleId, cursor) }.getOrNull() ?: return null
        return parsePage(response)?.takeIf { it.code == "ok" }
    }

    /** True when the ingest must abort the run. A duplicate or a provider-floor refusal is a skip. */
    private fun ingestAborts(context: NativeRcsReceiveContext, record: String): Boolean {
        val outcome = runCatching {
            core.ingest(context, record, NativeRcsObservationProvenance.HISTORICAL).also {
                GoogleMessagesLinkService.requestRcsMediaReceive()
            }
        }.getOrNull() ?: return true
        val refusal = listOfNotNull(outcome.recordOutcome, outcome.statusOutcome).firstOrNull { it.startsWith(REJECTED_PREFIX) } ?: return false
        return refusal != PROVIDER_FLOOR_REFUSAL
    }

    private fun write(block: () -> String): Boolean = runCatching(block).getOrNull() == RECORDED

    private fun active(cancelled: AtomicBoolean, current: () -> Boolean) = !cancelled.get() && enabled() && current()

    private fun lastActivity(handle: LinkHandle, contexts: List<NativeRcsReceiveContext>): Map<String, Long> {
        val activity = HashMap<String, Long>()
        contexts.map { it.googleConversationId }.distinct().chunked(MAX_ACTIVITY_IDS).forEach { ids ->
            val map = runCatching { JSONObject(handle.conversationLastActivity(JSONArray(ids).toString())) }.getOrNull()
                ?.takeIf { it.optString("code") == "ok" && it.optBoolean("complete") }
                ?.optJSONObject("last_message_ms") ?: return@forEach
            for (id in ids) if (map.has(id)) activity[id] = map.optLong(id)
        }
        return activity
    }

    /** Unknown or zero activity first, then most recent first. */
    private fun orderForSweep(contexts: List<NativeRcsReceiveContext>, activity: Map<String, Long>): List<NativeRcsReceiveContext> {
        val (known, unknown) = contexts.partition { (activity[it.googleConversationId] ?: 0L) > 0L }
        return unknown + known.sortedByDescending { activity.getValue(it.googleConversationId) }
    }

    private fun sweepStart(ordered: List<NativeRcsReceiveContext>): Int {
        val id = synchronized(sweepLock) { sweepContinuation } ?: return 0
        return ordered.indexOfFirst { it.googleConversationId == id } + 1
    }

    /** Only a known, positive activity time below the slack-adjusted watermark, with no open gap, is idle. */
    private fun isIdle(context: NativeRcsReceiveContext, lastMessageMs: Long?): Boolean =
        lastMessageMs != null && lastMessageMs > 0 && lastMessageMs < context.watermarkMs - SLACK_MS &&
            context.resumeHint == null && !context.incomplete

    private fun lock(conversationId: String) = locks.getOrPut(conversationId) { Any() }

    private fun parseLiveEvent(json: String): LiveEvent? = runCatching {
        val envelope = JSONObject(json)
        when (envelope.getString("code")) {
            "ok" -> envelope.getJSONObject("record").let { record ->
                LiveEvent.Record(record.getString("google_conversation_id"), record.toString())
            }
            "protocol_switch" -> LiveEvent.ProtocolSwitch(envelope.getString("google_conversation_id"))
            else -> null
        }
    }.getOrNull()

    private fun sentAtMs(record: String) = runCatching { JSONObject(record).optLong("sent_at_ms") }.getOrDefault(0L)

    private fun parsePage(json: String): Page? = runCatching {
        val value = JSONObject(json)
        val records = value.optJSONArray("records") ?: JSONArray()
        val signals = value.optJSONArray("signals") ?: JSONArray()
        Page(
            code = value.getString("code"),
            records = List(records.length()) { index ->
                records.getJSONObject(index).let { PageRecord(it.toString(), it.optLong("sent_at_ms")) }
            },
            signals = List(signals.length()) { index ->
                signals.getJSONObject(index).let { signal ->
                    PageSignal(signal.getString("google_conversation_id"), signal.optLong("sent_at_ms").takeIf { it > 0 })
                }
            },
            nextCursor = value.optString("next_cursor").takeIf(String::isNotEmpty),
            hasMore = value.optBoolean("has_more"),
        )
    }.getOrNull()

    private data class Page(
        val code: String,
        val records: List<PageRecord>,
        val signals: List<PageSignal>,
        val nextCursor: String?,
        val hasMore: Boolean,
    )

    private class PageRecord(val json: String, val sentAtMs: Long)
    private class PageSignal(val googleConversationId: String, val sentAtMs: Long?)

    private sealed interface LiveEvent {
        val googleConversationId: String
        data class Record(override val googleConversationId: String, val record: String) : LiveEvent
        data class ProtocolSwitch(override val googleConversationId: String) : LiveEvent
    }

    private companion object {
        const val SLACK_MS = 15 * 60 * 1000L
        const val TWO_MINUTES_MS = 2 * 60 * 1000L
        const val MAX_PAGES = 32
        const val SWEEP_CAP = 25
        const val MAX_ACTIVITY_IDS = 500
        const val RECORDED = "recorded"
        const val REJECTED_PREFIX = "rejected:"
        const val PROVIDER_FLOOR_REFUSAL = "rejected:provider_floor"
        const val UNPROVEN_REFUSAL = "rejected:conversation_unproven"
    }
}

internal interface RcsReceiveCore {
    fun context(googleId: String): NativeRcsReceiveContext?
    fun contexts(): List<NativeRcsReceiveContext>
    fun ingest(context: NativeRcsReceiveContext, record: String, provenance: NativeRcsObservationProvenance): NativeRcsIngestOutcome

    /** Fenced hint write, called strictly after the page's ingests. Returns `recorded` or `rejected:<code>`. */
    fun recordProgress(context: NativeRcsReceiveContext, targetMs: Long, cursor: String): String

    /** Fenced watermark advance to the maximum of the stored and [targetMs] values. Clears hint and gap. */
    fun complete(context: NativeRcsReceiveContext, targetMs: Long): String

    /** Fenced hint removal that keeps the gap open for a later trigger. */
    fun clearHint(context: NativeRcsReceiveContext): String
    fun invalidate(context: NativeRcsReceiveContext, googleId: String, eventAtMs: Long?): Boolean
}

private class GeneratedRcsReceiveCore(private val context: Context) : RcsReceiveCore {
    private fun client() = checkNotNull(NativeGateway.open(context))
    override fun context(googleId: String) = client().gatewayRcsReceiveContext(googleId)
    override fun contexts() = client().gatewayRcsLinkOwnedConversations()
    override fun ingest(context: NativeRcsReceiveContext, record: String, provenance: NativeRcsObservationProvenance) = if (provenance == NativeRcsObservationProvenance.LIVE) client().gatewayIngestGoRcsRecordJson(context.conversationId, context.ownershipGeneration, record) else client().gatewayIngestRcsProjectedObservation(context.conversationId, context.ownershipGeneration, record, provenance)
    override fun recordProgress(context: NativeRcsReceiveContext, targetMs: Long, cursor: String) = client().gatewayRecordRcsRecoveryProgress(context.conversationId, context.ownershipGeneration, targetMs, cursor)
    override fun complete(context: NativeRcsReceiveContext, targetMs: Long) = client().gatewayCompleteRcsRecovery(context.conversationId, context.ownershipGeneration, targetMs)
    override fun clearHint(context: NativeRcsReceiveContext) = client().gatewayClearRcsRecoveryHint(context.conversationId, context.ownershipGeneration)
    override fun invalidate(context: NativeRcsReceiveContext, googleId: String, eventAtMs: Long?) =
        client().gatewayInvalidateRcsContextForGoogleEvent(context.conversationId, googleId, context.expectation, eventAtMs) != null
}
