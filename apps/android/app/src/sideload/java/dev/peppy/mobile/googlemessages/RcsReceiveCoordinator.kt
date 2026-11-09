package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.gateway.NativeGateway
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeRcsIngestOutcome
import uniffi.peppy_mobile_bindings.NativeRcsObservationProvenance
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

internal class RcsReceiveCoordinator(
    context: Context,
    private val core: RcsReceiveCore = GeneratedRcsReceiveCore(context.applicationContext),
    private val enabled: () -> Boolean = { GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest == true },
) : RcsReceiveSink {
    private val locks = ConcurrentHashMap<String, Any>()

    override fun receive(recordJson: String) {
        if (!enabled()) return
        val event = parseLiveEvent(recordJson) ?: return
        val googleId = event.googleConversationId
        val context = runCatching { core.context(googleId) }.getOrNull() ?: return
        synchronized(lock(context.conversationId)) {
            if (!enabled()) return
            when (event) {
                is LiveEvent.Record -> runCatching {
                    core.ingest(context, event.record, NativeRcsObservationProvenance.LIVE)
                }
                is LiveEvent.ProtocolSwitch -> runCatching { core.invalidate(context, googleId) }
            }
        }
    }

    fun resume(
        handle: LinkHandle,
        cancelled: AtomicBoolean = AtomicBoolean(),
        current: () -> Boolean = { true },
    ) {
        if (!enabled() || !current()) return
        core.contexts().forEach { context -> if (!cancelled.get() && current()) page(handle, context, cancelled, current) }
    }

    private fun page(
        handle: LinkHandle,
        initial: NativeRcsReceiveContext,
        cancelled: AtomicBoolean,
        current: () -> Boolean,
    ) {
        synchronized(lock(initial.conversationId)) {
            var context = initial
            repeat(MAX_PAGES) {
                if (cancelled.get() || !enabled() || !current()) return
                val response = handle.fetchRcsGap(context.googleConversationId, context.historyToken.orEmpty())
                val page = parsePage(response) ?: return
                if (page.code != "ok") return
                for (record in page.records) {
                    val outcome = runCatching {
                        core.ingest(context, record, NativeRcsObservationProvenance.HISTORICAL)
                    }.getOrNull()
                    if (cancelled.get() || !enabled() || !current() || !outcome.complete()) return
                }
                for (signalGoogleId in page.signalGoogleIds) {
                    if (cancelled.get() || !enabled() || !current()) return
                    if (runCatching { core.invalidate(context, signalGoogleId) }.isFailure) return
                }
                if (cancelled.get() || !enabled() || !current() || page.nextCursor == null) return
                val checkpoint = runCatching { core.checkpoint(context, page.nextCursor) }.getOrNull() ?: return
                if (checkpoint.startsWith("rejected:")) return
                if (!page.hasMore) return
                context = runCatching { core.context(context.googleConversationId) }.getOrNull() ?: return
            }
        }
    }

    private fun lock(conversationId: String) = locks.getOrPut(conversationId) { Any() }
    private fun NativeRcsIngestOutcome?.complete() =
        this != null && !recordOutcome.startsWith("rejected:") && statusOutcome?.startsWith("rejected:") != true
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
    private fun parsePage(json: String): Page? = runCatching {
        val value = JSONObject(json); val records = value.optJSONArray("records") ?: JSONArray(); val signals = value.optJSONArray("signals") ?: JSONArray()
        Page(value.getString("code"), List(records.length()) { records.getJSONObject(it).toString() }, List(signals.length()) { signals.getJSONObject(it).getString("google_conversation_id") }, value.optString("next_cursor").takeIf(String::isNotEmpty), value.optBoolean("has_more"))
    }.getOrNull()
    private data class Page(val code: String, val records: List<String>, val signalGoogleIds: List<String>, val nextCursor: String?, val hasMore: Boolean)
    private sealed interface LiveEvent {
        val googleConversationId: String
        data class Record(override val googleConversationId: String, val record: String) : LiveEvent
        data class ProtocolSwitch(override val googleConversationId: String) : LiveEvent
    }
    private companion object { const val MAX_PAGES = 32 }
}

internal interface RcsReceiveCore {
    fun context(googleId: String): NativeRcsReceiveContext?
    fun contexts(): List<NativeRcsReceiveContext>
    fun ingest(context: NativeRcsReceiveContext, record: String, provenance: NativeRcsObservationProvenance): NativeRcsIngestOutcome
    fun checkpoint(context: NativeRcsReceiveContext, token: String): String
    fun invalidate(context: NativeRcsReceiveContext, googleId: String)
}

private class GeneratedRcsReceiveCore(private val context: Context) : RcsReceiveCore {
    private fun client() = checkNotNull(NativeGateway.open(context))
    override fun context(googleId: String) = client().gatewayRcsReceiveContext(googleId)
    override fun contexts() = client().gatewayRcsLinkOwnedConversations()
    override fun ingest(context: NativeRcsReceiveContext, record: String, provenance: NativeRcsObservationProvenance) = if (provenance == NativeRcsObservationProvenance.LIVE) client().gatewayIngestGoRcsRecordJson(context.conversationId, context.ownershipGeneration, record) else client().gatewayIngestRcsProjectedObservation(context.conversationId, context.ownershipGeneration, record, provenance)
    override fun checkpoint(context: NativeRcsReceiveContext, token: String) = client().gatewayCheckpointRcsHistory(context.conversationId, context.ownershipGeneration, token)
    override fun invalidate(context: NativeRcsReceiveContext, googleId: String) { client().gatewayInvalidateRcsContextForGoogleEvent(context.conversationId, googleId, context.expectation) }
}
