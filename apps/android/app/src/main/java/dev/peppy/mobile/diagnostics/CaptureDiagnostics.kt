package dev.peppy.mobile.diagnostics

import dev.peppy.mobile.SmsCaptureSummary
import dev.peppy.mobile.mms.MmsBackfillRecheckOutcome

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
    var providerFailureHighWaterPage = 0
    var providerFailureRowRead = 0
    var providerFailurePartTextRead = 0
    var providerFailurePartCopy = 0
    var providerFailureSecurity = 0
    var providerFailureIo = 0
    var providerFailureStateArgument = 0
    var providerFailureProvider = 0
    var providerFailureOther = 0
    var recheckVerified = 0
    var recheckProgress = 0
    var recheckAwaitingProvider = 0
    var recheckUnchangedUnavailable = 0
    var recheckUnchangedPartUnavailable = 0
    var recheckTransientFailure = 0
    var recheckProviderChanged = 0
    var recheckFeedbackFailed = 0
    var recheckIdentityMismatch = 0

    fun recordRecheckOutcome(outcome: MmsBackfillRecheckOutcome) {
        when (outcome) {
            MmsBackfillRecheckOutcome.VERIFIED -> recheckVerified++
            MmsBackfillRecheckOutcome.PROGRESS -> recheckProgress++
            MmsBackfillRecheckOutcome.AWAITING_PROVIDER -> recheckAwaitingProvider++
            MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE -> recheckUnchangedUnavailable++
            MmsBackfillRecheckOutcome.UNCHANGED_PART_UNAVAILABLE -> recheckUnchangedPartUnavailable++
            MmsBackfillRecheckOutcome.TRANSIENT_FAILURE -> recheckTransientFailure++
            MmsBackfillRecheckOutcome.PROVIDER_CHANGED -> recheckProviderChanged++
        }
    }

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
            SyncTraceField.PROVIDER_FAILURE_HIGH_WATER_PAGE to diagnostics.providerFailureHighWaterPage.toLong(),
            SyncTraceField.PROVIDER_FAILURE_ROW_READ to diagnostics.providerFailureRowRead.toLong(),
            SyncTraceField.PROVIDER_FAILURE_PART_TEXT_READ to diagnostics.providerFailurePartTextRead.toLong(),
            SyncTraceField.PROVIDER_FAILURE_PART_COPY to diagnostics.providerFailurePartCopy.toLong(),
            SyncTraceField.PROVIDER_FAILURE_SECURITY to diagnostics.providerFailureSecurity.toLong(),
            SyncTraceField.PROVIDER_FAILURE_IO to diagnostics.providerFailureIo.toLong(),
            SyncTraceField.PROVIDER_FAILURE_STATE_ARGUMENT to diagnostics.providerFailureStateArgument.toLong(),
            SyncTraceField.PROVIDER_FAILURE_OTHER to diagnostics.providerFailureOther.toLong(),
            SyncTraceField.PROVIDER_FAILURE_PROVIDER to diagnostics.providerFailureProvider.toLong(),
            SyncTraceField.RECHECK_VERIFIED to diagnostics.recheckVerified.toLong(),
            SyncTraceField.RECHECK_PROGRESS to diagnostics.recheckProgress.toLong(),
            SyncTraceField.RECHECK_AWAITING_PROVIDER to diagnostics.recheckAwaitingProvider.toLong(),
            SyncTraceField.RECHECK_UNCHANGED_UNAVAILABLE to diagnostics.recheckUnchangedUnavailable.toLong(),
            SyncTraceField.RECHECK_UNCHANGED_PART_UNAVAILABLE to diagnostics.recheckUnchangedPartUnavailable.toLong(),
            SyncTraceField.RECHECK_TRANSIENT_FAILURE to diagnostics.recheckTransientFailure.toLong(),
            SyncTraceField.RECHECK_PROVIDER_CHANGED to diagnostics.recheckProviderChanged.toLong(),
            SyncTraceField.RECHECK_FEEDBACK_FAILED to diagnostics.recheckFeedbackFailed.toLong(),
            SyncTraceField.RECHECK_IDENTITY_MISMATCH to diagnostics.recheckIdentityMismatch.toLong(),
            SyncTraceField.BUDGET_EXHAUSTED to if (diagnostics.budgetExhausted) 1 else 0,
            SyncTraceField.MORE to if (more()) 1 else 0,
            SyncTraceField.REASON to when {
                diagnostics.budgetExhausted -> SyncDiagnosticReason.BUDGET
                more() -> SyncDiagnosticReason.MORE
                else -> SyncDiagnosticReason.COMPLETE
            },
        )
    }, block)
