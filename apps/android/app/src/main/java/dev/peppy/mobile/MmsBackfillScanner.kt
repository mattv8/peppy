package dev.peppy.mobile

internal enum class MmsBackfillPhase { LIVE, HISTORY, RECHECK }
internal enum class MmsBackfillRecheckState { PENDING, BLOCKED, UNAVAILABLE, COMPLETE }
internal enum class MmsBackfillRecheckOutcome { PROGRESS, VERIFIED, UNCHANGED_UNAVAILABLE, UNCHANGED_PART_UNAVAILABLE, TRANSIENT_FAILURE, PROVIDER_CHANGED, AWAITING_PROVIDER }
internal data class MmsBackfillRecheckConditions(
    val charging: Boolean,
    val capacityChanged: Boolean,
    val explicitRefresh: Boolean,
    val providerChanged: Boolean,
)

internal data class MmsLegacyBackfillState(
    val historyBaseline: String?,
    val importHistory: Boolean?,
    val recheckCursor: String?,
)

internal data class MmsBackfillScanPlan(
    val phase: MmsBackfillPhase,
    val cursor: String?,
    val historyUpperBound: String?,
    val historyComplete: Boolean,
    val paused: Boolean,
    val budget: Int,
    val descending: Boolean = false,
    val historyFloor: String? = null,
)

internal data class MmsBackfillRecheckRequest(
    val providerMessageId: String,
    val incoming: Boolean,
    val acquisitionId: String = "",
    val state: MmsBackfillRecheckState = MmsBackfillRecheckState.PENDING,
)
internal data class MmsBackfillRecheck(val requests: List<MmsBackfillRecheckRequest>, val budget: Int, val continuationDue: Boolean = false)
internal data class MmsBackfillPageResult(
    val lastDurableId: String?,
    val exhausted: Boolean,
    val more: Boolean,
    val observations: Int = 0,
    val reason: String? = null,
)

internal interface MmsBackfillCore {
    fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState)
    fun plans(generation: String, scope: String, observedMaxId: String): List<MmsBackfillScanPlan>
    fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String)
    fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean
    fun dueRechecks(generation: String, scope: String, nowMs: Long, conditions: MmsBackfillRecheckConditions): MmsBackfillRecheck
    fun recordRecheckOutcome(acquisitionId: String, nowMs: Long, outcome: MmsBackfillRecheckOutcome, conditions: MmsBackfillRecheckConditions) = Unit
}

/**
 * Executes only the Rust-owned plan. Provider reads and acquisition writes are injected so no
 * preference checkpoint can become a second lifecycle authority.
 */
internal class MmsBackfillScanner(private val core: MmsBackfillCore) {
    fun run(
        generation: String,
        scope: String,
        legacy: MmsLegacyBackfillState,
        observedMaxId: String,
        nowMs: Long,
        recheckConditions: MmsBackfillRecheckConditions,
        scanPage: (MmsBackfillScanPlan) -> MmsBackfillPageResult,
        recheck: (MmsBackfillRecheckRequest) -> MmsBackfillRecheckOutcome?,
        historyAdmission: MmsHistoryCaptureAdmission? = null,
    ): Boolean {
        captureMmsStage(MmsDiagnosticStage.MIGRATE) { core.migrate(generation, scope, legacy) }
        var more = false
        val plans = captureMmsStage(MmsDiagnosticStage.PLANS) { core.plans(generation, scope, observedMaxId) }
        SyncDiagnostics.record(
            SyncTraceEvent.CAPTURE_PROGRESS,
            SyncTraceField.STAGE to MmsDiagnosticStage.PLANS,
            SyncTraceField.COUNT to plans.size.toLong(),
        )
        for (plan in plans) {
            if (plan.paused) {
                if (plan.phase == MmsBackfillPhase.HISTORY) {
                    historyAdmission.beginSafely()?.let { historyAdmission?.finish(it, 0, exhausted = false, reason = "capacity") }
                }
                continue
            }
            if (plan.phase == MmsBackfillPhase.RECHECK) {
                val requests = captureMmsStage(MmsDiagnosticStage.RECHECK_SELECTION_UNCLASSIFIED) {
                    core.dueRechecks(generation, scope, nowMs, recheckConditions)
                }
                SyncDiagnostics.record(
                    SyncTraceEvent.CAPTURE_PROGRESS,
                    SyncTraceField.STAGE to MmsDiagnosticStage.RECHECK_SELECTION_UNCLASSIFIED,
                    SyncTraceField.ROWS to requests.requests.size.toLong(),
                    SyncTraceField.COUNT to requests.budget.toLong(),
                )
                var recheckMore = false
                var completedSelected = false
                var actuallyAttempted = 0
                captureMmsStage(MmsDiagnosticStage.RECHECK_ATTEMPTS) {
                    requests.requests.forEach { request ->
                        val outcome = recheck(request) ?: return@forEach
                        actuallyAttempted++
                        core.recordRecheckOutcome(request.acquisitionId, nowMs, outcome, recheckConditions)
                        completedSelected = completedSelected || request.state == MmsBackfillRecheckState.COMPLETE
                        recheckMore = recheckMore || outcome == MmsBackfillRecheckOutcome.PROGRESS
                    }
                }
                SyncDiagnostics.record(
                    SyncTraceEvent.CAPTURE_PROGRESS,
                    SyncTraceField.STAGE to MmsDiagnosticStage.RECHECK_ATTEMPTS,
                    SyncTraceField.ROWS to requests.requests.size.toLong(),
                    SyncTraceField.PENDING to requests.requests.count { it.state == MmsBackfillRecheckState.PENDING }.toLong(),
                    SyncTraceField.COUNT to requests.requests.count { it.state == MmsBackfillRecheckState.COMPLETE }.toLong(),
                    SyncTraceField.PROCESSED to actuallyAttempted.toLong(),
                )
                more = more || recheckMore || (requests.continuationDue && completedSelected && recheckConditions.charging)
                continue
            }
            val admission = if (plan.phase == MmsBackfillPhase.HISTORY) historyAdmission.beginSafely() else null
            if (plan.phase == MmsBackfillPhase.HISTORY && plan.historyComplete) {
                admission?.let { historyAdmission?.finish(it, 0, exhausted = true) }
                continue
            }
            val admittedPlan = admission?.let { plan.copy(budget = minOf(plan.budget, it.budget)) } ?: plan
            val result = try {
                scanPage(admittedPlan)
            } catch (error: Exception) {
                admission?.let { historyAdmission.finishSafely(it, 0, exhausted = false, reason = "provider_error") }
                throw error
            }
            try {
                result.lastDurableId?.let { core.advance(generation, scope, plan.phase, it) }
                if (plan.phase == MmsBackfillPhase.HISTORY) {
                    val bound = requireNotNull(plan.historyUpperBound) { "history plan missing frozen bound" }
                    core.finish(generation, scope, plan.phase, bound, result.exhausted)
                }
            } catch (error: Exception) {
                admission?.let { historyAdmission.finishSafely(it, result.observations, exhausted = false, reason = "storage_error") }
                throw error
            }
            if (plan.phase == MmsBackfillPhase.HISTORY) {
                admission?.let { historyAdmission?.finish(it, result.observations, result.exhausted, result.reason) }
            }
            more = more || result.more || !result.exhausted
        }
        return more
    }
}

private fun MmsHistoryCaptureAdmission?.beginSafely(): MessageHistoryRequestCoordinator.Admission? =
    try {
        this?.begin()
    } catch (_: Exception) {
        null
    }

private fun MmsHistoryCaptureAdmission?.finishSafely(
    admission: MessageHistoryRequestCoordinator.Admission,
    observations: Int,
    exhausted: Boolean,
    reason: String,
) {
    try {
        this?.finish(admission, observations, exhausted, reason)
    } catch (_: Exception) {
        // Preserve the provider/checkpoint failure that caused this settlement attempt.
    }
}
