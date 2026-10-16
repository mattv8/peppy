package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MmsBackfillScannerTest {
    @Test fun migratesOnceThenAdvancesOnlyDurablePagesAndFinishesExhaustedHistory() {
        val core = FakeCore(
            listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, "10", "20", false, false, 32),
                MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "4", "20", false, false, 32),
            ),
        )
        val scanner = MmsBackfillScanner(core)

        val more = scanner.run("generation", "scope", MmsLegacyBackfillState(null, null, null), "20", { plan ->
            if (plan.phase == MmsBackfillPhase.LIVE) MmsBackfillPageResult("12", false, true)
            else MmsBackfillPageResult("20", true, false)
        }) { false }

        assertEquals(1, core.migrations)
        assertEquals(listOf(MmsBackfillPhase.LIVE to "12", MmsBackfillPhase.HISTORY to "20"), core.advanced)
        assertEquals(listOf("20"), core.finished)
        assertTrue(more)
    }

    @Test fun pausedPlansDoNotReadProvidersAndFairRechecksUseCoreRequests() {
        val core = FakeCore(
            listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, null, "9", false, true, 32),
                MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32),
            ),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true)), 32),
        )
        var pages = 0
        val rechecked = mutableListOf<String>()

        val more = MmsBackfillScanner(core).run("generation", "scope", MmsLegacyBackfillState(null, null, null), "9", {
            pages++
            MmsBackfillPageResult(null, true, false)
        }) { request -> rechecked += request.providerMessageId; true }

        assertEquals(0, pages)
        assertEquals(listOf("7"), rechecked)
        assertTrue(more)
        assertFalse(core.finished.isNotEmpty())
    }

    @Test fun recheckAttemptsEverySelectedRequestEvenWhenTheFirstNeedsFollowup() {
        val core = FakeCore(
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 32)),
            MmsBackfillRecheck(listOf(MmsBackfillRecheckRequest("7", true), MmsBackfillRecheckRequest("8", false)), 32),
        )
        val attempted = mutableListOf<String>()

        assertTrue(MmsBackfillScanner(core).run("generation", "scope", MmsLegacyBackfillState(null, null, null), "9", {
            error("recheck plans do not scan provider pages")
        }) { request ->
            attempted += request.providerMessageId
            request.providerMessageId == "7"
        })

        assertEquals(listOf("7", "8"), attempted)
    }

    private class FakeCore(
        private val plans: List<MmsBackfillScanPlan>,
        private val rechecks: MmsBackfillRecheck = MmsBackfillRecheck(emptyList(), 32),
    ) : MmsBackfillCore {
        var migrations = 0
        val advanced = mutableListOf<Pair<MmsBackfillPhase, String>>()
        val finished = mutableListOf<String>()
        override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) { migrations++ }
        override fun plans(generation: String, scope: String, observedMaxId: String) = plans
        override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) { advanced += phase to providerId }
        override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean { finished += bound; return true }
        override fun rechecks(generation: String, scope: String) = rechecks
    }
}
