package dev.peppy.mobile

/** Numeric, phase-level summaries for background work. */
internal object BackgroundDiagnostics {
    const val MMS_CLEANUP_PHASE = 20L

    fun mmsCleanup(marked: Long, unlinked: Long, pending: Long) {
        SyncDiagnostics.record(
            SyncTraceEvent.GATEWAY_PROGRESS,
            SyncTraceField.PHASE to MMS_CLEANUP_PHASE,
            SyncTraceField.COUNT to marked,
            SyncTraceField.PROCESSED to unlinked,
            SyncTraceField.PENDING to pending,
        )
    }
    internal data class GatewayReadiness(val ready: Boolean, val backfillPending: Boolean)

    /** Parses the readiness JSON already returned by the native compaction-state call. */
    fun gatewayReadiness(json: String): GatewayReadiness? {
        if (!SyncDiagnostics.enabled) return null
        return try {
            val value = org.json.JSONObject(json)
            val ready = value.opt("contacts_ready") as? Boolean ?: return null
            GatewayReadiness(ready, value.optString("state") == "backfill_pending")
        } catch (_: Exception) {
            null
        }
    }

    fun gateway(received: Long, snapshotReceived: Long, progressed: Long, uploaded: Long, snapshotRemaining: Long, readiness: GatewayReadiness?) {
        if (!SyncDiagnostics.enabled) return
        val fields = mutableListOf(
            SyncTraceField.STAGE to SyncDiagnosticStage.GATEWAY,
            SyncTraceField.ROWS to received,
            SyncTraceField.PENDING to snapshotReceived,
            SyncTraceField.PROCESSED to progressed,
            SyncTraceField.COUNT to uploaded,
            SyncTraceField.REMAINING to snapshotRemaining,
        )
        readiness?.let {
            fields += SyncTraceField.READY to if (it.ready) 1 else 0
            fields += SyncTraceField.BACKFILL_PENDING to if (it.backfillPending) 1 else 0
        }
        SyncDiagnostics.record(SyncTraceEvent.GATEWAY_PROGRESS, *fields.toTypedArray())
    }

    fun contact(full: Boolean, report: ContactPassReport) {
        if (!SyncDiagnostics.enabled) return
        SyncDiagnostics.record(
            SyncTraceEvent.CONTACT_PROGRESS,
            SyncTraceField.STAGE to if (full) SyncDiagnosticStage.CONTACT_FULL else SyncDiagnosticStage.CONTACT_INCREMENTAL,
            SyncTraceField.ROWS to report.observed.toLong(),
            SyncTraceField.CAPTURED to report.captured.toLong(),
            SyncTraceField.PROCESSED to (report.applied + report.deleted + report.failed + report.outcomeUnknown).toLong(),
            SyncTraceField.MORE to if (report.more) 1 else 0,
            SyncTraceField.SUCCESS to if (report.outcome == ContactPassReport.Outcome.COMPLETED) 1 else 0,
        )
    }

    internal data class SocketFrameCounts(val ready: Int = 0, val event: Int = 0, val resync: Int = 0, val other: Int = 0)

    fun socket(state: RemoteMessagingStatus, reconnects: Int, frames: SocketFrameCounts = SocketFrameCounts()) {
        if (!SyncDiagnostics.enabled) return
        val reason = when (state) {
            RemoteMessagingStatus.CONNECTED -> SyncDiagnosticReason.CONNECTED
            RemoteMessagingStatus.RECONNECTING -> SyncDiagnosticReason.RECONNECTING
            RemoteMessagingStatus.LOCKED -> SyncDiagnosticReason.LOCKED
        }
        SyncDiagnostics.record(
            SyncTraceEvent.SOCKET_STATE,
            SyncTraceField.STAGE to SyncDiagnosticStage.SOCKET,
            SyncTraceField.REASON to reason,
            SyncTraceField.COUNT to reconnects.toLong(),
            SyncTraceField.ROWS to frames.ready.toLong(),
            SyncTraceField.CAPTURED to frames.event.toLong(),
            SyncTraceField.PROCESSED to frames.resync.toLong(),
            SyncTraceField.PENDING to frames.other.toLong(),
        )
    }

    fun notification(active: Int, reconciled: Int, dismissed: Int) {
        if (!SyncDiagnostics.enabled) return
        SyncDiagnostics.record(
            SyncTraceEvent.NOTIFICATION_PROGRESS,
            SyncTraceField.STAGE to SyncDiagnosticStage.NOTIFICATION,
            SyncTraceField.ROWS to active.toLong(),
            SyncTraceField.PROCESSED to reconciled.toLong(),
            SyncTraceField.COUNT to dismissed.toLong(),
        )
    }
}
