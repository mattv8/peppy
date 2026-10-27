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

internal object MmsDiagnosticStage {
    const val HIGH_WATER = 11L
    const val MIGRATE = 12L
    const val PLANS = 13L
    const val SCAN_PAGE = 14L
    const val RECHECK_SELECTION_UNCLASSIFIED = 15L
    const val RECHECK_ATTEMPTS = 16L
}

/** Pass-level provider work is aggregated: no provider identifiers or content are emitted. */
internal class MmsPassDiagnostics {
    var rows = 0
    var processed = 0
    var captured = 0
    var pending = 0
    var unavailable = 0
    var providerReads = 0
    var providerFailures = 0
    var pageQueries = 0
    var providerDurationMs = 0L
    var providerCpuMs = 0L
    var budgetExhausted = false

    fun add(result: MmsCaptureScanDiagnostics) {
        rows += result.rows
        processed += result.processed
        captured += result.captured
        pending += result.pending
        unavailable += result.unavailable
        budgetExhausted = budgetExhausted || result.budgetExhausted
    }
}

internal data class MmsCaptureScanDiagnostics(
    val rows: Int,
    val processed: Int,
    val captured: Int,
    val pending: Int,
    val unavailable: Int,
    val budgetExhausted: Boolean,
)

internal fun <T> captureMmsStage(stage: Long, block: () -> T): T =
    SyncDiagnostics.measureStage(SyncTraceEvent.CAPTURE_PROGRESS, stage, block)

internal fun <T> captureMmsPass(diagnostics: MmsPassDiagnostics, more: () -> Boolean, block: () -> T): T =
    SyncDiagnostics.measureWithFields(SyncTraceEvent.MMS_CAPTURE_PASS, {
        arrayOf(
            SyncTraceField.STAGE to SyncDiagnosticStage.MMS,
            SyncTraceField.ROWS to diagnostics.rows.toLong(),
            SyncTraceField.PROCESSED to diagnostics.processed.toLong(),
            SyncTraceField.CAPTURED to diagnostics.captured.toLong(),
            SyncTraceField.PENDING to diagnostics.pending.toLong(),
            SyncTraceField.UNAVAILABLE to diagnostics.unavailable.toLong(),
            SyncTraceField.PROVIDER_READS to diagnostics.providerReads.toLong(),
            SyncTraceField.PROVIDER_FAILURES to diagnostics.providerFailures.toLong(),
            SyncTraceField.PAGE_QUERIES to diagnostics.pageQueries.toLong(),
            SyncTraceField.PROVIDER_DURATION_MS to diagnostics.providerDurationMs,
            SyncTraceField.PROVIDER_CPU_MS to diagnostics.providerCpuMs,
            SyncTraceField.BUDGET_EXHAUSTED to if (diagnostics.budgetExhausted) 1 else 0,
            SyncTraceField.MORE to if (more()) 1 else 0,
            SyncTraceField.REASON to when {
                diagnostics.budgetExhausted -> SyncDiagnosticReason.BUDGET
                more() -> SyncDiagnosticReason.MORE
                else -> SyncDiagnosticReason.COMPLETE
            },
        )
    }, block)
