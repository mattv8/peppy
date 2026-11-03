package dev.peppy.mobile.diagnostics

/** Numeric, content-free summaries of one phone-read reconciliation pass. */
internal object PhoneReadDiagnostics {
    const val SKIPPED_NO_CANDIDATE = 1L
    const val SKIPPED_UNCHANGED = 2L
    const val NO_MATCH = 3L
    const val BLOCKED = 4L
    const val ACTED = 5L
    const val ERROR = 6L
    const val COMPLETED_ONLY = 7L

    internal class Counters {
        var providerQueries = 0
        var rowsRead = 0L
        var providerDurationMs = 0L
    }

    fun pass(outcome: Long, counters: Counters) {
        if (!SyncDiagnostics.enabled) return
        SyncDiagnostics.record(
            SyncTraceEvent.PHONE_READ_PASS,
            SyncTraceField.OUTCOME to outcome,
            SyncTraceField.PROVIDER_READS to counters.providerQueries.toLong(),
            SyncTraceField.ROWS to counters.rowsRead,
            SyncTraceField.PROVIDER_DURATION_MS to counters.providerDurationMs,
        )
    }
}
