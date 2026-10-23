package dev.peppy.mobile

internal enum class MmsBackfillPhase { LIVE, HISTORY, RECHECK }

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

internal data class MmsBackfillRecheckRequest(val providerMessageId: String, val incoming: Boolean)
internal data class MmsBackfillRecheck(val requests: List<MmsBackfillRecheckRequest>, val budget: Int)
internal data class MmsBackfillPageResult(val lastDurableId: String?, val exhausted: Boolean, val more: Boolean)

internal interface MmsBackfillCore {
    fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState)
    fun plans(generation: String, scope: String, observedMaxId: String): List<MmsBackfillScanPlan>
    fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String)
    fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean
    fun rechecks(generation: String, scope: String): MmsBackfillRecheck
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
        scanPage: (MmsBackfillScanPlan) -> MmsBackfillPageResult,
        recheck: (MmsBackfillRecheckRequest) -> Boolean,
    ): Boolean {
        core.migrate(generation, scope, legacy)
        var more = false
        for (plan in core.plans(generation, scope, observedMaxId)) {
            if (plan.paused) continue
            if (plan.phase == MmsBackfillPhase.RECHECK) {
                val requests = core.rechecks(generation, scope)
                var recheckMore = false
                requests.requests.forEach { request ->
                    recheckMore = recheck(request) || recheckMore
                }
                more = more || recheckMore
                continue
            }
            if (plan.phase == MmsBackfillPhase.HISTORY && plan.historyComplete) continue
            val result = scanPage(plan)
            result.lastDurableId?.let { core.advance(generation, scope, plan.phase, it) }
            if (plan.phase == MmsBackfillPhase.HISTORY) {
                val bound = plan.historyUpperBound
                if (bound != null) core.finish(generation, scope, plan.phase, bound, result.exhausted)
            }
            more = more || result.more || !result.exhausted
        }
        return more
    }
}
