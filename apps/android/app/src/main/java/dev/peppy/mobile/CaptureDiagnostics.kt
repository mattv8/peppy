package dev.peppy.mobile

/** Measures a synchronous capture phase without extending trace context across suspensions. */
internal fun <T> captureTimed(stage: Long, block: () -> T): T =
    SyncDiagnostics.measureStage(SyncTraceEvent.CAPTURE_PROGRESS, stage, block)

/** Emits bounded, content-free capture progress only after the capture result already exists. */
internal fun captureProgress(stage: Long, summary: SmsCaptureSummary) {
    if (!SyncDiagnostics.enabled) return
    SyncDiagnostics.record(
        SyncTraceEvent.CAPTURE_PROGRESS,
        SyncTraceField.STAGE to stage,
        SyncTraceField.ROWS to summary.rowsQueried.toLong(),
        SyncTraceField.PROCESSED to summary.processed.toLong(),
        SyncTraceField.ADVANCED to if (summary.advanced) 1 else 0,
        SyncTraceField.BUDGET_EXHAUSTED to if (summary.budgetExhausted) 1 else 0,
        SyncTraceField.REASON to captureReason(summary),
        SyncTraceField.CAPTURED to summary.captured.toLong(),
        SyncTraceField.PENDING to summary.pending.toLong(),
        SyncTraceField.UNAVAILABLE to summary.unavailable.toLong(),
        SyncTraceField.PAUSED to summary.paused.toLong(),
    )
}

private fun captureReason(summary: SmsCaptureSummary): Long = when {
    summary.providerError -> SyncDiagnosticReason.PROVIDER_ERROR
    summary.coreError -> SyncDiagnosticReason.CORE_ERROR
    summary.budgetExhausted -> SyncDiagnosticReason.BUDGET
    summary.more -> SyncDiagnosticReason.MORE
    summary.processed > 0 || summary.advanced -> SyncDiagnosticReason.COMPLETE
    else -> SyncDiagnosticReason.UNKNOWN
}

internal fun captureMmsProgress(
    rowsQueried: Int,
    processed: Int,
    advanced: Boolean,
    budgetExhausted: Boolean,
    captured: Int,
    pending: Int,
    unavailable: Int,
    more: Boolean,
) {
    if (!SyncDiagnostics.enabled) return
    val reason = when {
        budgetExhausted -> SyncDiagnosticReason.BUDGET
        more -> SyncDiagnosticReason.MORE
        processed > 0 || advanced -> SyncDiagnosticReason.COMPLETE
        else -> SyncDiagnosticReason.UNKNOWN
    }
    SyncDiagnostics.record(
        SyncTraceEvent.CAPTURE_PROGRESS,
        SyncTraceField.STAGE to SyncDiagnosticStage.MMS,
        SyncTraceField.ROWS to rowsQueried.toLong(),
        SyncTraceField.PROCESSED to processed.toLong(),
        SyncTraceField.ADVANCED to if (advanced) 1 else 0,
        SyncTraceField.BUDGET_EXHAUSTED to if (budgetExhausted) 1 else 0,
        SyncTraceField.REASON to reason,
        SyncTraceField.CAPTURED to captured.toLong(),
        SyncTraceField.PENDING to pending.toLong(),
        SyncTraceField.UNAVAILABLE to unavailable.toLong(),
    )
}
