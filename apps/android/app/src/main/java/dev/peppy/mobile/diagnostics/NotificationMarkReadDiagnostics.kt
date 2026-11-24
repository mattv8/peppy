package dev.peppy.mobile.diagnostics

/** Numeric, content-free counts for one Mark as read pass. Keys, identifiers, and exception text never leave the executor. */
internal object NotificationMarkReadDiagnostics {
    internal class Counters {
        var attempted = 0
        var sent = 0
        var canceled = 0
        var failed = 0
    }

    /** Idle passes record nothing: a pass is logged only when it examined or failed a request. */
    fun pass(counters: Counters) {
        if (!SyncDiagnostics.enabled) return
        if (counters.attempted == 0 && counters.failed == 0) return
        SyncDiagnostics.record(
            SyncTraceEvent.NOTIFICATION_MARK_READ_PASS,
            SyncTraceField.MARK_READ_ATTEMPTED to counters.attempted.toLong(),
            SyncTraceField.MARK_READ_SENT to counters.sent.toLong(),
            SyncTraceField.MARK_READ_CANCELED to counters.canceled.toLong(),
            SyncTraceField.MARK_READ_FAILED to counters.failed.toLong(),
        )
    }
}
