package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.annotation.VisibleForTesting
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.GatewayEnqueueCause
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.gatewayRecordRcsExplicitUnlinkCompleted

/** Records content-free link lifecycle on a worker thread before linked status is surfaced. */
internal object LinkCoreBridge {
    @VisibleForTesting @Volatile internal var beforeLifecycleWriteForTest: ((LinkStatus) -> Unit)? = null
    private var lastFacts: LinkCapabilityFacts? = null
    private var lastRecordedAt = 0L
    private val observationGeneration = AtomicLong(0L)
    private val lifecycleExecutor = Executors.newSingleThreadExecutor()

    /** Called only from link worker threads; suppresses duplicate lifecycle observations for ten minutes. */
    fun recordIfNeeded(context: Context, status: LinkStatus): Boolean = lifecycleExecutor.submit<Boolean> {
        var linkReturned = false
        val recorded = synchronized(this) {
            val now = System.currentTimeMillis()
            if (status.facts() == lastFacts && now - lastRecordedAt < HEARTBEAT_MS) return@synchronized true
            if (!record(context, status)) return@synchronized false
            linkReturned = status.state == "linked" && lastFacts?.state != "linked"
            lastFacts = status.facts()
            lastRecordedAt = now
            true
        }
        if (recorded && linkReturned) enqueueGatewayPass(context)
        recorded
    }.get()

    /** Completes an owner-requested unlink only after publishing a fresh Unlinked lifecycle. */
    fun recordExplicitUnlinkCompleted(context: Context): Boolean = lifecycleExecutor.submit<Boolean> {
        synchronized(this) {
            val observedAtMs = System.currentTimeMillis()
            val status = LinkStatus(0, "unlinked", false, false, false, 0, "")
            if (!record(context, status, observedAtMs)) return@synchronized false
            lastFacts = status.facts()
            lastRecordedAt = observedAtMs
            runCatching {
                NativeGateway.open(context.applicationContext)?.let { client ->
                    gatewayRecordRcsExplicitUnlinkCompleted(client, observedAtMs, System.currentTimeMillis())
                } ?: throw IllegalStateException("gateway_unavailable")
            }.onSuccess {
                sweepOwnerUnlinkCutoverNow(context.applicationContext)
            }.onFailure {
                recordGoogleMessagesLinkEvent("link", "explicit_unlink_completion_failed", 0)
            }.isSuccess
        }
    }.get()

    fun recordPreStart(context: Context, pairingAttempt: Long? = null): Boolean {
        if (pairingAttempt != null && !LinkRuntime.ownsPairingAttempt(pairingAttempt)) return false
        invalidateObservations()
        return lifecycleExecutor.submit<Boolean> {
            synchronized(this) {
                if (pairingAttempt != null && !LinkRuntime.ownsPairingAttempt(pairingAttempt)) return@synchronized false
                val status = LinkStatus(0, "unknown", false, false, false, 0, "")
                if (!record(context, status)) return@synchronized false
                lastFacts = status.facts()
                lastRecordedAt = System.currentTimeMillis()
                true
            }
        }.get()
    }

    fun observationToken(): Long = observationGeneration.get()

    fun scheduleOwnerUnlinkCutover(context: Context) {
        runCatching { lifecycleExecutor.execute { sweepOwnerUnlinkCutoverNow(context.applicationContext) } }
            .onFailure { recordGoogleMessagesLinkEvent("link", "rcs_cutover_sweep_failed", 0) }
    }

    /** Called by the off-main UI accessor; core remains the authoritative alert source. */
    fun cutoverMayHaveMissedMessages(context: Context): Boolean {
        val alerts = cutoverAlertIds(context) ?: return false
        val preferences = cutoverPreferences(context)
        val acknowledged = preferences.getStringSet(CUTOVER_ACKNOWLEDGED_IDS, emptySet()).orEmpty()
        val currentAcknowledgements = acknowledged.intersect(alerts)
        if (currentAcknowledgements != acknowledged) {
            preferences.edit().putStringSet(CUTOVER_ACKNOWLEDGED_IDS, currentAcknowledgements).apply()
        }
        return alerts.any { it !in currentAcknowledgements }
    }

    fun clearCutoverMayHaveMissedMessages(context: Context) {
        cutoverAlertIds(context)?.let { cutoverPreferences(context).edit().putStringSet(CUTOVER_ACKNOWLEDGED_IDS, it).apply() }
    }

    private fun cutoverAlertIds(context: Context): Set<String>? = runCatching {
        NativeGateway.open(context.applicationContext)?.gatewayRcsCutoverAlerts()?.conversationIds?.toSet()
            ?: emptySet()
    }.getOrNull()

    fun invalidateObservations() { observationGeneration.incrementAndGet() }

    fun recordObservation(token: Long, lease: LinkHandleLease, context: Context, status: LinkStatus): Boolean {
        return lifecycleExecutor.submit<Boolean> {
            var linkReturned = false
            val recorded = synchronized(this) {
                if (LinkRuntime.pairingInProgress.get() || token != observationGeneration.get() || !LinkRuntime.ownsObservationLease(lease)) return@synchronized true
                val now = System.currentTimeMillis()
                if (status.facts() == lastFacts && now - lastRecordedAt < HEARTBEAT_MS) return@synchronized true
                if (!record(context, status)) return@synchronized false
                linkReturned = status.state == "linked" && lastFacts?.state != "linked"
                lastFacts = status.facts()
                lastRecordedAt = now
                true
            }
            if (recorded && linkReturned) enqueueGatewayPass(context)
            recorded
        }.get()
    }

    internal fun resetForTest() = synchronized(this) {
        beforeLifecycleWriteForTest = null
        lastFacts = null
        lastRecordedAt = 0L
        observationGeneration.set(0L)
    }

    fun record(context: Context, status: LinkStatus): Boolean = record(context, status, System.currentTimeMillis())

    private fun record(context: Context, status: LinkStatus, now: Long): Boolean {
        val state = when (status.state) {
            "linked" -> NativeRcsLinkState.LINKED
            "unlinked" -> NativeRcsLinkState.UNLINKED
            "needs_relink" -> NativeRcsLinkState.NEEDS_RELINK
            "signed_out" -> NativeRcsLinkState.SIGNED_OUT
            else -> NativeRcsLinkState.UNAVAILABLE
        }
        return runCatching {
            beforeLifecycleWriteForTest?.invoke(status)
            NativeGateway.open(context.applicationContext)?.recordRcsLinkLifecycle(
                state,
                status.state != "unknown",
                status.healthy,
                status.phoneResponding,
                now,
                now,
            ) != null
        }.onFailure { failure ->
            val reason = when (failure) {
                is MobileBindingsException.Database -> "database_failed"
                is MobileBindingsException.KeysUnavailable -> "keys_unavailable"
                is MobileBindingsException.InvalidRequest -> "invalid_request"
                else -> "other_failed"
            }
            recordGoogleMessagesLinkEvent("link_lifecycle", reason, 0L)
        }.getOrDefault(false)
    }

    private fun enqueueGatewayPass(context: Context) {
        runCatching {
            GatewayWork.enqueueAfterCapture(context.applicationContext, GatewayEnqueueCause.RCS_LINK)
        }
    }

    private fun sweepOwnerUnlinkCutoverNow(context: Context) {
        runCatching {
            val client = NativeGateway.open(context) ?: return
            while (true) {
                val candidates = client.gatewayRcsOwnerUnlinkCutoverCandidates()
                if (candidates.isEmpty()) return
                candidates.forEach { candidate ->
                    val cutover = client.gatewayBeginRcsOwnerUnlinkCutover(
                        candidate.conversationId,
                        candidate.ownershipGeneration,
                        candidate.lifecycleGeneration,
                    )
                    if (cutover.ambiguousDetectorAlert) {
                        val preferences = cutoverPreferences(context)
                        val acknowledgements = preferences.getStringSet(CUTOVER_ACKNOWLEDGED_IDS, emptySet()).orEmpty()
                        preferences.edit().putStringSet(CUTOVER_ACKNOWLEDGED_IDS, acknowledgements - candidate.conversationId).apply()
                        recordGoogleMessagesLinkEvent("link", "rcs_cutover_ambiguous", 0)
                    }
                }
            }
        }.onFailure { recordGoogleMessagesLinkEvent("link", "rcs_cutover_sweep_failed", 0) }
    }

    private fun cutoverPreferences(context: Context) =
        context.applicationContext.getSharedPreferences(CUTOVER_PREFERENCES, Context.MODE_PRIVATE)

    private const val HEARTBEAT_MS = 10 * 60 * 1000L
    private const val CUTOVER_PREFERENCES = "google-messages-link-cutover"
    private const val CUTOVER_ACKNOWLEDGED_IDS = "acknowledged_ids.v2"
}

private data class LinkCapabilityFacts(val state: String, val running: Boolean, val healthy: Boolean, val phoneResponding: Boolean, val stopCode: String)
private fun LinkStatus.facts() = LinkCapabilityFacts(state, running, healthy, phoneResponding, stopCode)
