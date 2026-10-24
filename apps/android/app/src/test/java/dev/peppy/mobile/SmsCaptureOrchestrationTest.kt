package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking

class SmsCaptureOrchestrationTest {
    @Test fun acquisitionIsDurableBeforeCheckpointAndPendingRowsAreRechecked() {
        val calls = mutableListOf<String>()
        val core = FakeCore(calls)
        val provider = FakeProvider(listOf(record(1), record(2)))
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, provider)
        assertEquals(2, summary.captured)
        assertEquals(listOf("begin:1", "complete:1", "checkpoint:1", "begin:2", "complete:2", "checkpoint:2"), calls)
    }

    @Test fun pendingRowsRotatePastTheFirstFifty() {
        val calls = mutableListOf<String>()
        val acquisitions = (1..1_001).map { id -> acquisition("%04d".format(id), id.toLong()) }
        val core = FakeCore(calls, acquisitions)
        val provider = FakeProvider((1L..1_001L).map(::record))

        val first = SmsCapture.run(SmsCapturePlan("shared-generation", false, 100), core, provider)
        val second = SmsCapture.run(SmsCapturePlan("shared-generation", false, 100, pendingRecheckCursor = first.pendingRecheckCursor), core, provider)

        assertEquals("0050", first.pendingRecheckCursor)
        assertTrue(calls.contains("complete:51"))
        assertEquals("0100", second.pendingRecheckCursor)
    }

    @Test fun historyStopsAtItsFrozenAscendingBaseline() {
        val calls = mutableListOf<String>()
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", true, 0, historyUpperBound = 2),
            FakeCore(calls),
            FakeProvider(listOf(record(1), record(2), record(3))),
        )
        assertEquals(2, summary.captured)
        assertEquals(listOf("begin:1", "complete:1", "checkpoint:1", "begin:2", "complete:2", "checkpoint:2"), calls)
    }

    @Test fun deletedFrozenHistoryBoundIsExplicitlyExhausted() {
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", true, 0, historyUpperBound = 2),
            FakeCore(mutableListOf()),
            FakeProvider(emptyList()),
        )
        assertTrue(summary.historyExhausted)
        assertTrue(!summary.more)
    }

    @Test fun descendingHistoryCheckpointsNewestToOldestAndExhaustsBelowFloor() {
        val calls = mutableListOf<String>()
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", true, null, historyUpperBound = 5, descending = true, historyFloor = 2),
            FakeCore(calls),
            FakeProvider((1L..5L).map(::record)),
        )

        assertEquals(listOf("checkpoint:5", "checkpoint:4", "checkpoint:3"), calls.filter { it.startsWith("checkpoint:") })
        assertTrue(summary.historyExhausted)
    }

    @Test fun unknownSubscriptionDoesNotHoldCheckpointOrStarveLaterRows() {
        val calls = mutableListOf<String>()
        val rows = (1L..60L).map(::record).toMutableList()
        rows[1] = rows[1].copy(subscriptionId = null)
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(calls), FakeProvider(rows))
        assertEquals(50, summary.captured)
        assertTrue(calls.contains("checkpoint:50"))
        assertTrue(calls.contains("begin:2"))
    }

    @Test fun identicalLegitimateRowsRemainSeparateProviderAcquisitions() {
        val calls = mutableListOf<String>()
        val same = record(1).copy(body = "same", dateMs = 10)
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(calls), FakeProvider(listOf(same, same.copy(id = 2))))
        assertEquals(2, summary.captured)
        assertEquals(listOf("begin:1", "complete:1", "checkpoint:1", "begin:2", "complete:2", "checkpoint:2"), calls)
    }

    @Test fun immutableEvidenceStaysStableWhileOutboxPayloadSettlesAndChangesForReusedIds() {
        val original = record(7).copy(type = 4, body = "draft", dateSentMs = null)
        val settled = original.copy(type = 2, body = "sent", dateSentMs = 1234)
        val reused = settled.copy(dateMs = 2000)
        assertEquals(SmsCapture.sourceRowFingerprint(original), SmsCapture.sourceRowFingerprint(settled))
        assertTrue(SmsCapture.sourceRowFingerprint(settled) != SmsCapture.sourceRowFingerprint(reused))
    }

    @Test fun fullPageRequestsBoundedContinuation() {
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(mutableListOf()), FakeProvider((1L..50L).map(::record)))
        assertEquals(50, summary.captured)
        assertTrue(summary.more)
    }

    @Test fun tenSecondBudgetStopsBeforeAnUnprocessedRow() {
        val times = ArrayDeque(listOf(0L, 10_000_000_000L))
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", false, 0),
            FakeCore(mutableListOf()),
            FakeProvider(listOf(record(1))),
        ) { times.removeFirst() }
        assertEquals(0, summary.captured)
        assertEquals(0, summary.rowsQueried)
        assertTrue(summary.more)
        assertEquals(0, summary.processed)
        assertTrue(summary.budgetExhausted)
        assertTrue(!summary.advanced)
    }

    @Test fun completedRowsReportDurableCaptureProgress() {
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", false, 0),
            FakeCore(mutableListOf()),
            FakeProvider(listOf(record(1))),
        )

        assertEquals(1, summary.processed)
        assertEquals(1, summary.rowsQueried)
        assertTrue(summary.advanced)
        assertTrue(!summary.budgetExhausted)
    }

    @Test fun diagnosticsEmitStagedProgressWithoutChangingCaptureEffects() {
        val disabledCalls = mutableListOf<String>()
        val enabledCalls = mutableListOf<String>()
        val events = mutableListOf<String>()

        val disabled = SyncDiagnostics.withTestConfiguration(false, { error("clock") }, { error("cpu") }, { error("sink") }) {
            SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(disabledCalls), FakeProvider(listOf(record(1))))
        }
        val enabled = SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(enabledCalls), FakeProvider(listOf(record(1))))
        }

        assertEquals(disabled, enabled)
        assertEquals(disabledCalls, enabledCalls)
        assertTrue(events.any { it.contains("event=${SyncTraceEvent.CAPTURE_PROGRESS.code}") && it.contains("${SyncTraceField.STAGE.code}=${SyncDiagnosticStage.SMS_LIVE}") })
    }

    @Test fun invalidPendingProviderIdAdvancesTheRecheckCursorWithoutAProviderQuery() {
        val pending = acquisition("one", 1).copy(input = acquisition("one", 1).input.copy(providerMessageId = "invalid"))

        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 100), FakeCore(mutableListOf(), listOf(pending)), FakeProvider(emptyList()))

        assertEquals("one", summary.pendingRecheckCursor)
        assertEquals(1, summary.processed)
        assertEquals(0, summary.rowsQueried)
        assertTrue(summary.advanced)
    }

    @Test fun providerFailureRequestsFollowupInsteadOfReportingEmptySuccess() {
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), FakeCore(mutableListOf()), object : SmsCaptureProvider {
            override fun page(afterId: Long, limit: Int): SmsProviderPage = throw SmsProviderException("denied")
            override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): SmsProviderPage = throw SmsProviderException("denied")
            override fun read(id: Long): SmsProviderRecord? = throw SmsProviderException("denied")
        })
        assertTrue(summary.providerError)
        assertTrue(summary.more)
    }

    @Test fun providerFailurePreservesTheDurablePendingCursor() {
        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", false, 100, pendingRecheckCursor = "one"),
            FakeCore(mutableListOf(), listOf(acquisition("one", 1))),
            object : SmsCaptureProvider {
                override fun page(afterId: Long, limit: Int): SmsProviderPage = throw SmsProviderException("denied")
                override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): SmsProviderPage = throw SmsProviderException("denied")
                override fun read(id: Long): SmsProviderRecord? = throw SmsProviderException("denied")
            },
        )
        assertTrue(summary.providerError)
        assertEquals("one", summary.pendingRecheckCursor)
    }

    @Test fun outboxAcquisitionIsUpdatedThenCompletedWhenProviderReportsSent() {
        val calls = mutableListOf<String>()
        val provider = MutableProvider(record(1).copy(type = 4, body = "draft", dateMs = 10, dateSentMs = 11, creator = "messaging"))
        val core = UpdatingCore(calls)

        val first = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, provider)
        provider.row = provider.row.copy(type = 2, body = "sent", dateMs = 20, dateSentMs = 21)
        val second = SmsCapture.run(SmsCapturePlan("shared-generation", false, 1, pendingRecheckCursor = first.pendingRecheckCursor), core, provider)

        assertEquals(1, first.pending)
        assertEquals(1, second.captured)
        assertEquals(listOf("begin:draft", "state:provider_box_pending", "checkpoint:1", "begin:sent", "complete:1"), calls)
        assertEquals(21L, core.input!!.dateSentMs)
        assertEquals("messaging", core.input!!.creator)
    }

    @Test fun failedThenQueuedThenSentRemainsRecheckableWithoutPublishingDraft() {
        val calls = mutableListOf<String>()
        val provider = MutableProvider(record(1).copy(type = 5, body = "draft"))
        val core = UpdatingCore(calls)

        val failed = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, provider)
        provider.row = provider.row.copy(type = 6)
        val queued = SmsCapture.run(SmsCapturePlan("shared-generation", false, 1, pendingRecheckCursor = failed.pendingRecheckCursor), core, provider)
        provider.row = provider.row.copy(type = 2, body = "sent")
        val sent = SmsCapture.run(SmsCapturePlan("shared-generation", false, 1, pendingRecheckCursor = queued.pendingRecheckCursor), core, provider)

        assertEquals(1, failed.pending)
        assertEquals(1, queued.pending)
        assertEquals(1, sent.captured)
        assertEquals(1, calls.count { it.startsWith("complete:") })
    }

    @Test fun scannerSerializationAllowsOnlyOneRunnerAtATime() = runBlocking {
        var active = 0
        var maximum = 0
        val workers = List(2) {
            async(Dispatchers.Default) {
                SmsCaptureWork.serialized {
                    active += 1
                    maximum = maxOf(maximum, active)
                    Thread.sleep(20)
                    active -= 1
                }
            }
        }
        workers.awaitAll()
        assertEquals(1, maximum)
    }

    @Test fun changedSubscriptionOnRecheckIsUnavailable() {
        val existing = acquisition("one", 1).copy(input = acquisition("one", 1).input.copy(subscriptionId = SimRoutes.routeId(1)))
        val calls = mutableListOf<String>()
        val provider = FakeProvider(listOf(record(1).copy(subscriptionId = 2)))
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 100), FakeCore(calls, listOf(existing)), provider)
        assertEquals(1, summary.unavailable)
        assertTrue(calls.contains("state:one:UNAVAILABLE"))
    }

    @Test fun completedHistoryStillRechecksPendingAcquisitions() {
        val calls = mutableListOf<String>()
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", true, 10, historyComplete = true), FakeCore(calls, listOf(acquisition("one", 1))), FakeProvider(listOf(record(1))))
        assertEquals(1, summary.captured)
        assertEquals(listOf("begin:1", "complete:1"), calls)
    }

    @Test fun reconciliationReobservesReusedIdsAndStopsAtItsFrozenBound() {
        val calls = mutableListOf<String>()
        val replacement = record(7).copy(body = "replacement", dateMs = 2000)
        val result = SmsCapture.reconcile(
            SmsReconciliationPlan("shared-generation", SmsCapture.PROVIDER_SCOPE, 0, 7),
            FakeCore(calls),
            FakeProvider(listOf(replacement)),
        )
        assertTrue(result.exhausted)
        assertTrue(!result.summary.more)
        assertEquals(1, result.summary.captured)
        assertEquals(listOf("begin:7", "complete:7"), calls)
    }

    @Test fun coreUnavailableAcquisitionIsTerminalWithoutCompletionLoop() {
        val calls = mutableListOf<String>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
                calls += "begin:${input.providerMessageId}"
                return SmsAcquisition("x", input, SmsAcquisitionState.UNAVAILABLE, "reconciliation_ambiguous")
            }
        }
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, FakeProvider(listOf(record(1))))
        assertEquals(1, summary.unavailable)
        assertTrue(!summary.more && !summary.coreError && !summary.providerError)
        assertEquals(listOf("begin:1", "checkpoint:1"), calls)
    }

    @Test fun completionFailureIsDurablePendingAndBacksOffInsteadOfTightLooping() {
        val calls = mutableListOf<String>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun complete(id: String): SmsCompletion = throw IllegalStateException("database busy")
        }
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, FakeProvider(listOf(record(1))))
        assertTrue(summary.coreError)
        assertTrue(!summary.more && !summary.providerError)
        assertEquals(listOf("begin:1", "checkpoint:1"), calls) // no PENDING reset, no immediate append
    }

    @Test fun coreFailureOnBeginIsNotAProviderOutageAndHoldsTheCheckpoint() {
        val calls = mutableListOf<String>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition = throw IllegalStateException("core")
        }
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, FakeProvider(listOf(record(1), record(2))))
        assertTrue(summary.coreError)
        assertTrue(!summary.providerError)
        assertTrue(calls.none { it.startsWith("checkpoint:") })
    }

    @Test fun capacityBackpressureStopsWithoutProviderErrorOrSkippingRows() {
        val calls = mutableListOf<String>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition =
                if (input.providerMessageId == "2") throw SmsCaptureCapacityException() else FakeCore(calls).begin(input)
        }
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", true, 0, historyUpperBound = 3), core, FakeProvider(listOf(record(1), record(2), record(3))))
        assertEquals(1, summary.paused)
        assertTrue(!summary.providerError && !summary.coreError && !summary.historyExhausted && !summary.more)
        assertTrue(calls.contains("checkpoint:1"))
        assertTrue(calls.none { it == "checkpoint:2" || it == "begin:3" })
    }

    @Test fun descendingHistoryStopsAtTheFirstNonDurableRow() {
        val calls = mutableListOf<String>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition =
                if (input.providerMessageId == "4") throw SmsCaptureCapacityException() else FakeCore(calls).begin(input)
        }

        val summary = SmsCapture.run(
            SmsCapturePlan("shared-generation", true, null, historyUpperBound = 5, descending = true),
            core,
            FakeProvider((1L..5L).map(::record)),
        )

        assertEquals(listOf("checkpoint:5"), calls.filter { it.startsWith("checkpoint:") })
        assertTrue(!summary.historyExhausted)
        assertTrue(calls.none { it == "begin:3" })
    }

    @Test fun pausedHistoryPlanStillDrainsPendingRows() {
        val calls = mutableListOf<String>()
        val summary = SmsCapture.run(SmsCapturePlan("shared-generation", true, 0, historyUpperBound = 5, paused = true), FakeCore(calls, listOf(acquisition("one", 1))), FakeProvider(listOf(record(1))))
        assertEquals(1, summary.paused)
        assertEquals(1, summary.captured)
        assertEquals(listOf("begin:1", "complete:1"), calls)
    }

    @Test fun draftsAndFailedSendsAreRecheckedUnderParkedReasons() {
        val calls = mutableListOf<String>()
        val reasons = mutableListOf<String?>()
        val core = object : SmsCaptureCore by FakeCore(calls) {
            override fun setState(id: String, state: SmsAcquisitionState, reason: String?) { reasons += reason }
        }
        SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, FakeProvider(listOf(record(1).copy(type = 3), record(2).copy(type = 5))))
        assertEquals(listOf(SmsCapture.REASON_DRAFT, SmsCapture.REASON_FAILED), reasons)
    }

    @Test fun sweepClassifiesRowsAtOrBelowTheHistoryBaselineAsImported() {
        val imported = mutableMapOf<String, Boolean>()
        val core = object : SmsCaptureCore by FakeCore(mutableListOf()) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
                imported[input.providerMessageId] = input.imported
                return SmsAcquisition(input.providerMessageId, input, SmsAcquisitionState.PENDING, null)
            }
        }
        SmsCapture.reconcile(SmsReconciliationPlan("shared-generation", SmsCapture.PROVIDER_SCOPE, 0, 102, historyUpperBound = 100), core, FakeProvider(listOf(record(99), record(100), record(101))))
        assertEquals(mapOf("99" to true, "100" to true, "101" to false), imported)
    }

    @Test fun reconciliationPassesDurablyPausedRows() {
        val core = object : SmsCaptureCore by FakeCore(mutableListOf()) {
            override fun begin(input: SmsAcquisitionInput) = SmsAcquisition("paused", input, SmsAcquisitionState.PAUSED, null)
        }

        val result = SmsCapture.reconcile(
            SmsReconciliationPlan("shared-generation", SmsCapture.PROVIDER_SCOPE, 0, 2),
            core,
            FakeProvider(listOf(record(1), record(2))),
        )

        assertEquals(2L, result.cursor)
        assertTrue(result.exhausted)
    }

    @Test fun incomingRowsCarryTheReceiverBroadcastIdentity() {
        val inputs = mutableListOf<SmsAcquisitionInput>()
        val core = object : SmsCaptureCore by FakeCore(mutableListOf()) {
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
                inputs += input
                return SmsAcquisition(input.providerMessageId, input, SmsAcquisitionState.PENDING, null)
            }
        }
        val inbox = record(1).copy(address = "+15557654321", body = "first half, second half", dateMs = 1_700_000_003_000, dateSentMs = 1_700_000_000_000)
        SmsCapture.run(SmsCapturePlan("shared-generation", false, 0), core, FakeProvider(listOf(inbox, inbox.copy(id = 2, type = 2))))
        assertEquals(IncomingCapture.broadcastId("+15557654321", 1_700_000_000_000, "first half, second half"), inputs[0].broadcastId)
        assertEquals(null, inputs[1].broadcastId) // outgoing rows never claim a broadcast identity
    }

    private fun record(id: Long) = SmsProviderRecord(id, 1, 1, "+15551234", "body", 1000, 1001, 1, true, true, null, null)
    private fun acquisition(id: String, providerId: Long) = SmsAcquisition(id, SmsAcquisitionInput("shared-generation", SimRoutes.routeId(1), providerId.toString(), "1", true, "+15551234", emptyList(), "body", false, 1000, 1001, null), SmsAcquisitionState.PENDING, null)

    private class FakeProvider(private val rows: List<SmsProviderRecord>) : SmsCaptureProvider {
        override fun page(afterId: Long, limit: Int) = SmsProviderPage(rows.filter { it.id > afterId }.take(limit), rows.lastOrNull()?.id ?: afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): SmsProviderPage {
            val ceiling = cursor ?: upperBound
            val records = rows.filter { it.id <= ceiling && it.id > (floor ?: 0L) }.sortedByDescending { it.id }.take(limit)
            return SmsProviderPage(records, records.lastOrNull()?.id ?: ceiling)
        }
        override fun read(id: Long) = rows.firstOrNull { it.id == id }
    }

    private class FakeCore(private val calls: MutableList<String>, private val pending: List<SmsAcquisition> = emptyList()) : SmsCaptureCore {
        override fun acquisitions(afterId: String?, limit: Int): List<SmsAcquisition> {
            val ordered = pending.sortedBy { it.id }
            val index = afterId?.let { saved -> ordered.indexOfFirst { it.id == saved } } ?: -1
            return (ordered.drop(index + 1) + ordered.take(index + 1)).take(limit)
        }
        override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
            calls += "begin:${input.providerMessageId}"
            return SmsAcquisition(input.providerMessageId, input, SmsAcquisitionState.PENDING, null)
        }
        override fun complete(id: String): SmsCompletion { calls += "complete:$id"; return SmsCompletion.CAPTURED }
        override fun setState(id: String, state: SmsAcquisitionState, reason: String?) { calls += "state:$id:$state" }
        override fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String) { calls += "checkpoint:$providerId" }
    }

    private class MutableProvider(var row: SmsProviderRecord) : SmsCaptureProvider {
        override fun page(afterId: Long, limit: Int) = if (row.id > afterId) SmsProviderPage(listOf(row), row.id) else SmsProviderPage(emptyList(), afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
            row.takeIf { it.id <= (cursor ?: upperBound) && it.id > (floor ?: 0L) }?.let { SmsProviderPage(listOf(it), it.id) }
                ?: SmsProviderPage(emptyList(), cursor ?: upperBound)
        override fun read(id: Long) = row.takeIf { it.id == id }
    }

    private class UpdatingCore(private val calls: MutableList<String>) : SmsCaptureCore {
        private var acquisition: SmsAcquisition? = null
        var input: SmsAcquisitionInput? = null
        override fun acquisitions(afterId: String?, limit: Int) = listOfNotNull(acquisition).filter { it.state == SmsAcquisitionState.PENDING }
        override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
            this.input = input
            calls += "begin:${input.body}"
            return (acquisition ?: SmsAcquisition("1", input, SmsAcquisitionState.PENDING, null)).also { acquisition = it.copy(input = input) }
        }
        override fun complete(id: String): SmsCompletion { calls += "complete:$id"; acquisition = acquisition?.copy(state = SmsAcquisitionState.COMPLETE); return SmsCompletion.CAPTURED }
        override fun setState(id: String, state: SmsAcquisitionState, reason: String?) { calls += "state:$reason"; acquisition = acquisition?.copy(state = state, reason = reason) }
        override fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String) { calls += "checkpoint:$providerId" }
    }
}
