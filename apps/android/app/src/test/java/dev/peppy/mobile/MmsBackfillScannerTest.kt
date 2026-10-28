package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MmsBackfillScannerTest {
    private val idle = MmsBackfillRecheckConditions(charging = false, capacityChanged = false, explicitRefresh = false, providerChanged = false)
    private val charging = idle.copy(charging = true)

    @Test fun migratesOnceThenAdvancesOnlyDurablePagesAndFinishesExhaustedHistory() {
        val core = FakeCore(
            listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, "10", "20", false, false, 32),
                MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "4", "20", false, false, 32),
            ),
        )

        val more = MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "20", 1L, idle,
            scanPage = { plan ->
                if (plan.phase == MmsBackfillPhase.LIVE) MmsBackfillPageResult("12", false, true)
                else MmsBackfillPageResult("20", true, false)
            },
            recheck = { error("no recheck plan") },
        )

        assertEquals(1, core.migrations)
        assertEquals(listOf(MmsBackfillPhase.LIVE to "12", MmsBackfillPhase.HISTORY to "20"), core.advanced)
        assertEquals(listOf("20"), core.finished)
        assertTrue(more)
    }

    @Test fun remotePauseFlagsRunLiveAndRecheckButNotHistory() {
        val core = FakeCore(
            listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, null, "9", false, false, 32),
                MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, null, "9", false, true, 32),
                MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32),
            ),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true)), 32, continuationDue = true),
        )
        var pages = 0
        val rechecked = mutableListOf<String>()

        val more = MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = {
                pages++
                MmsBackfillPageResult(null, true, false)
            },
            recheck = { request ->
                rechecked += request.providerMessageId
                MmsBackfillRecheckOutcome.PROGRESS
            },
        )

        assertEquals(1, pages)
        assertEquals(listOf("7"), rechecked)
        assertTrue(more)
        assertTrue(core.finished.isEmpty())
    }

    @Test fun recheckAttemptsEverySelectedRequestEvenWhenTheFirstNeedsFollowup() {
        val core = FakeCore(
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32)),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true), MmsBackfillRecheckRequest("8", false)), 32, continuationDue = true),
        )
        val attempted = mutableListOf<String>()

        val more = MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { error("recheck plans do not scan provider pages") },
            recheck = { request ->
                attempted += request.providerMessageId
                if (request.providerMessageId == "7") MmsBackfillRecheckOutcome.PROGRESS else MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE
            },
        )

        assertEquals(listOf("7", "8"), attempted)
        assertTrue(more)
    }

    @Test fun unattemptedRechecksAreNotRecordedAsOutcomes() {
        val core = FakeCore(
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32)),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true)), 32, continuationDue = true),
        )

        val more = MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { error("recheck plans do not scan provider pages") },
            recheck = { null },
        )

        assertTrue(core.outcomes.isEmpty())
        assertFalse(more)
    }

    @Test fun recheckAttemptDiagnosticsAggregateSelectedStatesAndRecordedAttempts() {
        val events = mutableListOf<String>()
        val requests = MmsBackfillRecheck(
            listOf(
                MmsBackfillRecheckRequest("7", true, state = MmsBackfillRecheckState.PENDING),
                MmsBackfillRecheckRequest("8", true, state = MmsBackfillRecheckState.COMPLETE),
                MmsBackfillRecheckRequest("9", true, state = MmsBackfillRecheckState.UNAVAILABLE),
            ),
            32,
        )

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsBackfillScanner(FakeCore(listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32)), requests)).run(
                "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
                scanPage = { error("recheck plans do not scan provider pages") },
                recheck = { request -> if (request.providerMessageId == "9") null else MmsBackfillRecheckOutcome.PROGRESS },
            )
        }

        val counts = events.map(::traceFields).single {
            it["event"] == SyncTraceEvent.CAPTURE_PROGRESS.code &&
                it[SyncTraceField.STAGE.code.toString()] == MmsDiagnosticStage.RECHECK_ATTEMPTS &&
                it.containsKey(SyncTraceField.ROWS.code.toString())
        }
        assertEquals(3L, counts.getValue(SyncTraceField.ROWS.code.toString()))
        assertEquals(1L, counts.getValue(SyncTraceField.PENDING.code.toString()))
        assertEquals(1L, counts.getValue(SyncTraceField.COUNT.code.toString()))
        assertEquals(2L, counts.getValue(SyncTraceField.PROCESSED.code.toString()))
    }

    @Test fun providerChangedDirectionIsRecordedButDoesNotContinue() {
        val core = FakeCore(
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32)),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true)), 32, continuationDue = true),
        )

        val more = MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { error("recheck plans do not scan provider pages") },
            recheck = { MmsBackfillRecheckOutcome.PROVIDER_CHANGED },
        )

        assertEquals(listOf(MmsBackfillRecheckOutcome.PROVIDER_CHANGED), core.outcomes.map { it.second })
        assertFalse(more)
    }

    @Test fun completedMaintenanceContinuesOnlyWhileChargingAndAfterAnAttempt() {
        val completed = MmsBackfillRecheckRequest("7", true, state = MmsBackfillRecheckState.COMPLETE)
        val rechecks = MmsBackfillRecheck(listOf(completed), 32, continuationDue = true)
        val plans = listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32))

        val chargingMore = MmsBackfillScanner(FakeCore(plans, rechecks)).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, charging,
            scanPage = { error("unused") },
            recheck = { MmsBackfillRecheckOutcome.VERIFIED },
        )
        val idleMore = MmsBackfillScanner(FakeCore(plans, rechecks)).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { error("unused") },
            recheck = { MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE },
        )
        val budgetOnlyMore = MmsBackfillScanner(FakeCore(plans, rechecks)).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, charging,
            scanPage = { error("unused") },
            recheck = { null },
        )

        assertTrue(chargingMore)
        assertFalse(idleMore)
        assertFalse(budgetOnlyMore)
    }

    @Test fun admittedHistoryClampsExistingPageThenFinishesAfterCheckpoint() {
        val calls = mutableListOf<String>()
        val admission = object : MmsHistoryCaptureAdmission {
            override fun begin() = MessageHistoryRequestCoordinator.Admission("token", 7)
            override fun finish(admission: MessageHistoryRequestCoordinator.Admission, observations: Int, exhausted: Boolean, reason: String?) {
                calls += "finish:$observations:$exhausted:${reason ?: "none"}"
            }
        }
        val core = FakeCore(listOf(MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "3", "9", false, false, 50)))

        MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { plan ->
                assertEquals(7, plan.budget)
                calls += "scan"
                MmsBackfillPageResult("5", exhausted = true, more = false, observations = 6)
            },
            recheck = { error("unused") },
            historyAdmission = admission,
        )

        assertEquals(listOf(MmsBackfillPhase.HISTORY to "5"), core.advanced)
        assertEquals(listOf("9"), core.finished)
        assertEquals(listOf("scan", "finish:6:true:none"), calls)
    }


    @Test fun admittedHistorySettlesAfterDurableCallsEvenWhenCoreReportsNotExhausted() {
        val calls = mutableListOf<String>()
        val core = FakeCore(
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "3", "9", false, false, 50)),
            finishResult = false,
            calls = calls,
        )
        val admission = recordingAdmission(calls)

        MmsBackfillScanner(core).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = {
                calls += "scan"
                MmsBackfillPageResult("5", exhausted = false, more = true, observations = 7)
            },
            recheck = { error("unused") },
            historyAdmission = admission,
        )

        assertEquals(listOf("begin", "scan", "advance", "core-finish", "finish:7:false:none"), calls)
    }

    @Test fun admittedHistorySettlesProviderFailureBeforeRethrowingScanError() {
        val calls = mutableListOf<String>()
        val failure = IllegalStateException("provider failed")

        val thrown = runCatching {
            MmsBackfillScanner(FakeCore(listOf(MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "3", "9", false, false, 50)))).run(
                "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
                scanPage = { throw failure },
                recheck = { error("unused") },
                historyAdmission = recordingAdmission(calls),
            )
        }.exceptionOrNull()

        assertTrue(thrown === failure)
        assertEquals(listOf("begin", "finish:0:false:provider_error"), calls)
    }

    @Test fun checkpointFailureSettlesStorageErrorBeforeRethrowing() {
        val calls = mutableListOf<String>()
        val failure = IllegalStateException("checkpoint failed")
        val delegate = FakeCore(listOf(MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "3", "9", false, false, 50)))
        val core = object : MmsBackfillCore by delegate {
            override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) {
                throw failure
            }
        }

        val thrown = runCatching {
            MmsBackfillScanner(core).run(
                "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
                scanPage = { MmsBackfillPageResult("5", exhausted = false, more = true, observations = 4) },
                recheck = { error("unused") },
                historyAdmission = recordingAdmission(calls),
            )
        }.exceptionOrNull()

        assertTrue(thrown === failure)
        assertEquals(listOf("begin", "finish:4:false:storage_error"), calls)
    }

    @Test fun pausedAndCompleteHistoryPlansSettleZeroWorkAdmissions() {
        val calls = mutableListOf<String>()
        val plans = listOf(
            MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, null, "9", false, true, 50),
            MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, null, "9", true, false, 50),
        )

        MmsBackfillScanner(FakeCore(plans)).run(
            "generation", "scope", MmsLegacyBackfillState(null, null, null), "9", 1L, idle,
            scanPage = { error("zero-work plans must not scan") },
            recheck = { error("unused") },
            historyAdmission = recordingAdmission(calls),
        )

        assertEquals(
            listOf("begin", "finish:0:false:capacity", "begin", "finish:0:true:none"),
            calls,
        )
    }

    private fun recordingAdmission(calls: MutableList<String>) = object : MmsHistoryCaptureAdmission {
        override fun begin() = MessageHistoryRequestCoordinator.Admission("token", 100).also { calls += "begin" }
        override fun finish(admission: MessageHistoryRequestCoordinator.Admission, observations: Int, exhausted: Boolean, reason: String?) {
            calls += "finish:$observations:$exhausted:${reason ?: "none"}"
        }
    }

    private class FakeCore(
        private val plans: List<MmsBackfillScanPlan>,
        private val rechecks: MmsBackfillRecheck = MmsBackfillRecheck(emptyList(), 32),
        private val finishResult: Boolean = true,
        private val calls: MutableList<String>? = null,
    ) : MmsBackfillCore {
        var migrations = 0
        val advanced = mutableListOf<Pair<MmsBackfillPhase, String>>()
        val finished = mutableListOf<String>()
        val outcomes = mutableListOf<Pair<String, MmsBackfillRecheckOutcome>>()
        override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) { migrations++ }
        override fun plans(generation: String, scope: String, observedMaxId: String) = plans
        override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) {
            calls?.add("advance")
            advanced += phase to providerId
        }
        override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean {
            calls?.add("core-finish")
            finished += bound
            return finishResult
        }
        override fun dueRechecks(generation: String, scope: String, nowMs: Long, conditions: MmsBackfillRecheckConditions) = rechecks
        override fun recordRecheckOutcome(acquisitionId: String, nowMs: Long, outcome: MmsBackfillRecheckOutcome, conditions: MmsBackfillRecheckConditions) {
            outcomes += acquisitionId to outcome
        }
    }

    private fun traceFields(line: String): Map<String, Long> = line.split(' ').associate { token ->
        val (key, value) = token.split('=', limit = 2)
        key to value.toLong()
    }
}
