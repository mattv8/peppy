package dev.peppy.mobile

import android.graphics.Bitmap
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.work.Data
import androidx.work.ListenableWorker
import androidx.work.workDataOf
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import kotlinx.coroutines.CancellationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeMmsAcquisition
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeMmsBeginOutcome
import uniffi.peppy_mobile_bindings.NativeMmsProviderIdentityQuarantine
import uniffi.peppy_mobile_bindings.NativeMmsPreviewState
import uniffi.peppy_mobile_bindings.NativeMmsSource

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MmsCaptureOrchestrationTest {
    @Test fun diagnosticsEmitContentFreeMmsProgress() {
        val events = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCapture.run(plan(), FakeCore(), FakeProvider(listOf(record(1))), temp())
        }

        assertTrue(events.any { it.contains("event=${SyncTraceEvent.CAPTURE_PROGRESS.code}") && it.contains("${SyncTraceField.STAGE.code}=${SyncDiagnosticStage.MMS}") && it.contains("${SyncTraceField.PROCESSED.code}=") })
    }

    @Test fun diagnosticsAggregateProviderWorkWithoutMmsContentOrProviderIdentifiers() {
        val events = mutableListOf<String>()
        val sensitive = record(77).copy(subject = "private subject", parts = listOf(
            MmsProviderPart(991, 0, "text/plain", null, 106, "private body", Uri.parse("content://mms/part/991")),
        ))

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCapture.run(plan(), FakeCore(), FakeProvider(listOf(sensitive)), temp())
        }

        val passes = events.filter { it.startsWith("event=${SyncTraceEvent.MMS_CAPTURE_PASS.code} ") }
        assertEquals(1, passes.size)
        assertTrue(passes.single().contains("${SyncTraceField.PROVIDER_READS.code}="))
        assertTrue(passes.single().contains("${SyncTraceField.PAGE_QUERIES.code}="))
        assertTrue(events.none { it.contains("private subject") || it.contains("private body") || it.contains("991") || it.contains("77") })
    }

    @Test fun corePlanDrivesLiveAndHistoryProviderScansWithoutPreferenceCheckpoints() {
        val core = FakeCore()
        val provider = FakeProvider(listOf(record(1), record(2)))

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, core.migrations)
        assertEquals(listOf(MmsBackfillPhase.LIVE to "2", MmsBackfillPhase.HISTORY to "2"), core.advanced)
        assertEquals(listOf("2"), core.finished)
        assertEquals(2, result.captured)
    }

    @Test fun unknownProviderDatesReachCoreAsUnknownSentinel() {
        val core = FakeCore()
        MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(dateMs = null))), temp())
        assertEquals(0L, core.inputs.first().observedAtMs)
    }

    @Test fun livePlanRemainsUnboundedWhenItsBaselineIsAlsoHistoryMetadata() {
        val core = FakeCore(initialLiveCursor = "100", historyUpperBound = "100")

        MmsCapture.run(plan(), core, FakeProvider(listOf(record(101))), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "101"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "101" && !it.imported })
    }

    @Test fun descendingHistoryStartsWithNullableCursorAndAdvancesToLowestDurableId() {
        val core = FakeCore(historyUpperBound = "5", historyDescending = true, historyFloor = "2")
        val provider = FakeProvider((1L..5L).map(::record))

        MmsCapture.run(plan(), core, provider, temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.HISTORY to "3"))
    }

    @Test fun descendingHistoryRecordsMissingSubscriptionsAsUnavailableAndContinues() {
        val core = FakeCore(historyUpperBound = "5", historyDescending = true, historyFloor = "2")
        val records = (3L..5L).map(::record).toMutableList()
        records[0] = records[0].copy(subscriptionId = null)

        MmsCapture.run(plan(), core, FakeProvider(records), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.HISTORY to "3"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "3" && it.source.subscriptionId == "android-mms-unknown-subscription" })
    }

    @Test fun shortDescendingPageWithBlockedCheckpointDoesNotFinishHistory() {
        val core = FakeCore(
            initialLiveCursor = "5",
            historyUpperBound = "5",
            historyDescending = true,
            historyFloor = "2",
            quarantinedProviderIds = setOf("5"),
            failBeginProviderIds = setOf("4"),
        )

        MmsCapture.run(plan(), core, FakeProvider((3L..5L).map(::record)), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.HISTORY to "5"))
        assertTrue(core.finished.isEmpty())
        assertTrue(core.inputs.none { it.source.providerMessageId == "3" })
    }

    @Test fun partialLivePageWithMissingSubscriptionIsExhaustedWithoutAContinuationLoop() {
        val core = FakeCore(historyUpperBound = "0")
        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(subscriptionId = null))), temp())

        assertFalse(result.more)
    }

    @Test fun mappedOriginalPartIsNotCopiedAgainWhenOptionalMetadataIsAbsent() {
        val core = FakeCore(mappedParts = mapOf("10" to "attachment-10"))
        val provider = FakeProvider(listOf(record(1)))

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(0, provider.copyCount)
        assertEquals(1, core.completed)
    }

    @Test fun recheckLetsCoreResolveProviderIdentityWithoutAnAcquisitionSnapshot() {
        val core = FakeCore(
            recheckRequests = listOf(
                MmsBackfillRecheckRequest("1", true),
                MmsBackfillRecheckRequest("2", true),
            ),
        )

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1), record(2))), temp())

        assertEquals(listOf("1", "2"), core.inputs.map { it.source.providerMessageId })
        assertTrue(result.more)
    }

    @Test fun manualRecheckUsesAcceptedCoreBoundAndResetFailureDoesNotAbortScanning() {
        val core = FakeCore(
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)),
            failManualRecheck = true,
        )

        val result = MmsCapture.run(plan().copy(manualRecheck = true), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(32, core.manualRecheckLimit)
        assertEquals(listOf("1"), core.inputs.map { it.source.providerMessageId })
        assertTrue(result.more)
    }

    @Test fun mappedDeferredPreviewWaitsForCapacityThenCompletesWithoutPreparingOriginalAgain() {
        val part = MmsProviderPart(10, 0, "image/png", "photo.png", null, null, Uri.parse("content://mms/part/10"))
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(part))), imageBytes = png())
        val core = FakeCore(
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
            pendingBytes = 512L * 1024 * 1024,
        )

        val deferred = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(0, deferred.captured)
        assertTrue(deferred.pending > 0)
        assertEquals(0, core.completed)
        assertTrue(core.preparedMediaTypes.isEmpty())
        assertEquals(NativeMmsPreviewState.DEFERRED, core.previewStates("ignored")["10"])

        core.pendingBytes = 0
        val resumed = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, core.completed)
        assertEquals(1, resumed.captured)
        assertEquals(listOf("image/jpeg"), core.preparedMediaTypes)
        assertFalse(core.preparedMediaTypes.contains("image/png"))
        assertEquals(NativeMmsPreviewState.READY, core.previewStates("ignored")["10"])
    }

    @Test fun mappedPreviewReadFailureRemainsDeferredAndPending() {
        val part = MmsProviderPart(10, 0, "image/png", "photo.png", null, null, Uri.parse("content://mms/part/10"))
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(part))), failCopy = true)
        val core = FakeCore(
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
        )

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(0, core.completed)
        assertTrue(result.pending > 0)
        assertEquals(NativeMmsPreviewState.DEFERRED, core.previewStates("ignored")["10"])
    }

    @Test fun typedQuarantineIsDurableAndDoesNotPinRowsPastAPage() {
        val core = FakeCore(quarantinedProviderIds = setOf("1"))
        val provider = FakeProvider((1L..51L).map(::record))

        MmsCapture.run(plan(), core, provider, temp())
        MmsCapture.run(plan(), core, provider, temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "50"))
        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "51"))
    }

    @Test fun postSettleAcquisitionWithStableRowSkipsSettleResetInSamePass() {
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        val core = FakeCore(initialReason = "preview_deferred")

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertTrue(core.reasons.none { it?.startsWith("settling") == true })
        assertEquals(1, core.completed)
        assertEquals(1, provider.copyCount)
        assertFalse(result.more)
    }

    @Test fun postSettleAcquisitionWithMissingMappedPartResettlesInsteadOfProcessing() {
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        val core = FakeCore(initialReason = "preview_deferred", mappedParts = mapOf("99" to "attachment-99"))

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertTrue(core.reasons.first()?.startsWith("settling:") == true)
        assertTrue(result.more)
    }

    @Test fun providerContentPendingStillSettlesBeforeProcessing() {
        val core = FakeCore(initialReason = "provider_content_pending")

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertTrue(core.reasons.first()?.startsWith("settling:") == true)
        assertTrue(result.more)
    }

    @Test fun mappedDeferredPreviewWithoutThumbnailHeadroomIsNotRecopied() {
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        val core = FakeCore(
            initialReason = "preview_deferred",
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
            pendingBytes = 512L * 1024 * 1024,
        )

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(0, provider.copyCount)
        assertEquals(0, core.completed)
        assertEquals(NativeMmsPreviewState.DEFERRED, core.previewStates("ignored")["10"])
        assertTrue(core.reasons.all { it == "preview_deferred" })
        assertFalse(result.more)
    }

    @Test fun mappedDeferredPreviewWithThumbnailHeadroomIsRecopied() {
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        val core = FakeCore(
            initialReason = "preview_deferred",
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
        )

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, provider.copyCount)
        assertEquals(1, core.completed)
        assertEquals(listOf("image/jpeg"), core.preparedMediaTypes)
    }

    @Test fun oversizePreviewBecomesPermanentlyUnavailableAndAcquisitionCompletes() {
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png(), previewLimit = 100)
        val core = FakeCore()

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, core.completed)
        assertEquals(NativeMmsPreviewState.PERMANENTLY_UNAVAILABLE, core.previewStates("ignored")["10"])
    }

    @Test fun providerHintReachesOnlyTheHintedRecheckPassWithoutAddingWork() {
        val hinted = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))
        val periodic = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))

        val hintedResult = MmsCapture.run(plan().copy(providerChanged = true), hinted, FakeProvider(listOf(record(1))), temp())
        val periodicResult = MmsCapture.run(plan(), periodic, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(true), hinted.dueConditions.map { it.providerChanged })
        assertEquals(listOf(false), periodic.dueConditions.map { it.providerChanged })
        assertEquals(periodicResult.more, hintedResult.more)
        assertEquals(periodicResult.captured, hintedResult.captured)
    }

    @Test fun providerContentPendingRecheckIsRecordedAsAwaitingProviderWithoutContinuation() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))
        val pendingRecord = record(1).copy(ready = false, error = "provider_content_pending")

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(pendingRecord)), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.AWAITING_PROVIDER), core.recheckOutcomes)
        assertFalse(result.more)
    }

    @Test fun failedUnmappedPartCopyRecordsOneUnchangedPartUnavailableOutcome() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)), initialReason = "preview_deferred")
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), failCopy = true)

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.UNCHANGED_PART_UNAVAILABLE), core.recheckOutcomes)
        assertFalse(result.more)
    }

    @Test fun settlingRecheckIsRecordedAsProgressForImmediateFollowup() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.PROGRESS), core.recheckOutcomes)
        assertTrue(result.more)
    }

    @Test fun directionChangeIsRecordedWithoutForcingAFollowup() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", false)))

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.PROVIDER_CHANGED), core.recheckOutcomes)
        assertTrue(core.inputs.isEmpty())
        assertFalse(result.more)
    }

    @Test fun completeIdentityRecheckIsVerifiedInsteadOfTransient() {
        val core = FakeCore(
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", true, state = MmsBackfillRecheckState.COMPLETE)),
            initialState = NativeMmsAcquisitionState.COMPLETE,
        )

        MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.VERIFIED), core.recheckOutcomes)
    }

    @Test fun twoHintsBeforeAPassScheduleOneDebouncedWork() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestHint(sink)
        scheduler.requestHint(sink)

        assertEquals(listOf(androidx.work.ExistingWorkPolicy.KEEP to true), scheduled)
    }

    @Test fun normalQueuedPassAbsorbsHintAfterPriorPassCompletes() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.finishPass(managedWork = true)
        scheduler.requestNormal(sink)
        scheduler.requestHint(sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun manualReplacementAbsorbsHintUntilReplacementStarts() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.replace(sink)
        scheduler.requestHint(sink)

        assertEquals(listOf(androidx.work.ExistingWorkPolicy.REPLACE to false), scheduled)
    }

    @Test fun hintDuringRunningPassCreatesExactlyOneDelayedSuccessor() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.requestHint(sink)
        scheduler.requestHint(sink)
        scheduler.requestContinuation(actualMore = true, sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun continuationRequiresActualMoreAndKeepsTheExistingDebounce() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.requestContinuation(actualMore = false, sink)
        scheduler.requestContinuation(actualMore = true, sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun periodicPassDoesNotClearQueuedOneTimeHint() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.requestHint(sink)
        scheduler.beginPass(managedWork = false)
        scheduler.requestHint(sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun periodicPassRecoversAStaleQueueAfterFailedManagedChain() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.requestHint(sink)
        scheduler.finishPass(managedWork = true)
        scheduler.beginPass(managedWork = false)
        scheduler.requestHint(sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
                androidx.work.ExistingWorkPolicy.KEEP to true,
            ),
            scheduled,
        )
    }

    @Test fun retryPassClearsFailedQueuedHintSuccessor() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.beginPass(managedWork = true)
        scheduler.requestHint(sink)
        scheduler.finishPass(managedWork = true)
        scheduler.beginPass(managedWork = true)
        scheduler.requestHint(sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.KEEP to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun managedDataRecognizesHeadOneTimeRequestsButNotEmptyPeriodicRequests() {
        assertTrue(MmsCaptureWork.isManagedData(workDataOf(MmsCaptureWork.MANUAL_RECHECK to false)))
        assertFalse(MmsCaptureWork.isManagedData(Data.EMPTY))
    }

    @Test fun durableHintIsConsumedAndFailureRestoresItForTheRetryPass() {
        val context = hintContext()
        val input = workDataOf(MmsCaptureWork.MANAGED_WORK to true)
        MmsCaptureHints.record(context)

        val failed = MmsCaptureWork.runWorkerPass(context, input) { _, _ -> throw IOException("provider unavailable") }
        var providerChanged = false
        val retried = MmsCaptureWork.runWorkerPass(context, input) { _, changed ->
            providerChanged = changed
            MmsCaptureSummary(0, 0, 0, false)
        }

        assertEquals(ListenableWorker.Result.retry(), failed)
        assertEquals(ListenableWorker.Result.success(), retried)
        assertTrue(providerChanged)
        assertFalse(MmsCaptureHints.consumeProviderChange(context))
    }

    @Test fun cancellationRestoresConsumedHintAndPropagates() {
        val context = hintContext()
        MmsCaptureHints.record(context)
        var cancelled = false

        try {
            MmsCaptureWork.runWorkerPass(context, workDataOf(MmsCaptureWork.MANAGED_WORK to true)) { _, _ ->
                throw CancellationException("stopped")
            }
        } catch (_: CancellationException) {
            // Structured cancellation remains observable to WorkManager.
            cancelled = true
        }

        assertTrue(cancelled)
        assertTrue(MmsCaptureHints.consumeProviderChange(context))
    }

    private fun pngPart(id: Long) = MmsProviderPart(id, 0, "image/png", "photo.png", null, null, Uri.parse("content://mms/part/$id"))

    private fun plan() = MmsCapturePlan("generation", null, false, null, null, false)
    private fun temp() = kotlin.io.path.createTempDirectory("mms-capture-test").toFile()
    private fun hintContext(): android.content.Context = ApplicationProvider.getApplicationContext<android.content.Context>().also {
        it.getSharedPreferences(MmsCaptureHints.PREFERENCES, android.content.Context.MODE_PRIVATE).edit().clear().commit()
    }
    private fun record(id: Long) = MmsProviderRecord(
        id, 4, 7, 1, 1_000, null, "subject", listOf("+15550001"), emptyList(), listOf("+15550002"), emptyList(),
        132, null, listOf(MmsProviderPart(id * 10, 0, "text/plain", null, 106, "body", Uri.parse("content://mms/part/$id"))), true, null,
    )

    private class FakeProvider(
        private val records: List<MmsProviderRecord>,
        private val imageBytes: ByteArray? = null,
        private val failCopy: Boolean = false,
        private val previewLimit: Int = MmsMediaPreview.MAX_JPEG_BYTES,
    ) : MmsCaptureProvider {
        var copyCount = 0
        override fun inspectMedia(file: File, contentType: String?) = MmsMediaPreview(previewLimit).inspect(file, contentType)
        override fun highWater() = records.maxOfOrNull { it.id } ?: 0
        override fun page(afterId: Long, limit: Int) = MmsProviderPage(records.filter { it.id > afterId }.take(limit), afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): MmsProviderPage {
            val ceiling = cursor ?: upperBound
            val page = records.filter { it.id <= ceiling && it.id > (floor ?: 0L) }.sortedByDescending { it.id }.take(limit)
            return MmsProviderPage(page, page.lastOrNull()?.id ?: ceiling)
        }
        override fun read(id: Long) = records.firstOrNull { it.id == id }
        override fun readPartText(part: MmsProviderPart, byteLimit: Int) = part.text
        override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long): Long {
            copyCount++
            if (failCopy) error("provider read failed")
            val bytes = imageBytes ?: byteArrayOf()
            destination.writeBytes(bytes)
            return bytes.size.toLong()
        }
    }

    private class FakeCore(
        initialLiveCursor: String = "0",
        private val historyUpperBound: String? = null,
        private val historyDescending: Boolean = false,
        private val historyFloor: String? = null,
        private val mappedParts: Map<String, String> = emptyMap(),
        private val recheckRequests: List<MmsBackfillRecheckRequest> = emptyList(),
        private val quarantinedProviderIds: Set<String> = emptySet(),
        private val failBeginProviderIds: Set<String> = emptySet(),
        private val failManualRecheck: Boolean = false,
        initialPreviewStates: Map<String, NativeMmsPreviewState> = emptyMap(),
        var pendingBytes: Long = 0,
        private val initialReason: String = "settling",
        private val initialState: NativeMmsAcquisitionState = NativeMmsAcquisitionState.PENDING,
    ) : MmsCaptureCore {
        var migrations = 0
        val reasons = mutableListOf<String?>()
        val advanced = mutableListOf<Pair<MmsBackfillPhase, String>>()
        val finished = mutableListOf<String>()
        val inputs = mutableListOf<NativeMmsAcquisitionInput>()
        var completed = 0
        var manualRecheckLimit: Int? = null
        val preparedMediaTypes = mutableListOf<String>()
        private val previewStates = initialPreviewStates.toMutableMap()
        private val acquisitions = mutableMapOf<String, NativeMmsAcquisition>()
        private var liveCursor = initialLiveCursor
        override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) { migrations++ }
        override fun plans(generation: String, scope: String, observedMaxId: String) = if (recheckRequests.isNotEmpty()) {
            listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 50))
        } else {
            listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, liveCursor, historyUpperBound ?: observedMaxId, false, false, 50),
                MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, if (historyDescending) null else "0", historyUpperBound ?: observedMaxId, false, false, 50, historyDescending, historyFloor),
            )
        }
        override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) {
            advanced += phase to providerId
            if (phase == MmsBackfillPhase.LIVE) liveCursor = providerId
        }
        override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean { if (exhausted) finished += bound; return exhausted }
        val dueConditions = mutableListOf<MmsBackfillRecheckConditions>()
        val recheckOutcomes = mutableListOf<MmsBackfillRecheckOutcome>()
        override fun dueRechecks(generation: String, scope: String, nowMs: Long, conditions: MmsBackfillRecheckConditions): MmsBackfillRecheck {
            dueConditions += conditions
            return MmsBackfillRecheck(recheckRequests, 50, continuationDue = recheckRequests.isNotEmpty())
        }
        override fun recordRecheckOutcome(acquisitionId: String, nowMs: Long, outcome: MmsBackfillRecheckOutcome, conditions: MmsBackfillRecheckConditions) {
            recheckOutcomes += outcome
        }
        override fun begin(input: NativeMmsAcquisitionInput): NativeMmsBeginOutcome {
            inputs += input
            if (input.source.providerMessageId in failBeginProviderIds) error("core unavailable")
            val acquisition = acquisitions.getOrPut(input.source.providerMessageId) {
                NativeMmsAcquisition("acq-${input.source.providerMessageId}", "conversation", input, initialState, initialReason, emptyList())
            }
            return if (input.source.providerMessageId in quarantinedProviderIds) {
                NativeMmsBeginOutcome.Quarantined(NativeMmsProviderIdentityQuarantine(input.source, input.incoming, "ambiguous"))
            } else {
                NativeMmsBeginOutcome.Acquired(acquisition)
            }
        }
        override fun requestManualRecheck(generation: String, scope: String, limit: Int): Int {
            manualRecheckLimit = limit
            if (failManualRecheck) error("reset unavailable")
            return 0
        }
        override fun setState(id: String, state: NativeMmsAcquisitionState, reason: String?) {
            val entry = acquisitions.entries.firstOrNull { it.value.acquisitionId == id } ?: return
            reasons += reason
            entry.setValue(entry.value.copy(state = state, reason = reason))
        }
        override fun mappedParts(id: String) = mappedParts
        override fun previewStates(id: String) = previewStates.toMap()
        override fun pendingMediaBytes() = pendingBytes
        override fun prepareAttachment(path: String, mediaType: String, name: String): String {
            preparedMediaTypes += mediaType
            return "attachment-${preparedMediaTypes.size}"
        }
        override fun mapPart(id: String, providerPartId: String, attachmentId: String) = Unit
        override fun setPartMetadata(id: String, providerPartId: String, width: Int?, height: Int?, durationMs: Long?, thumbnailAttachmentId: String?) = Unit
        override fun setPartPreviewState(id: String, providerPartId: String, state: NativeMmsPreviewState) {
            previewStates[providerPartId] = state
        }
        override fun discardAttachment(id: String) = Unit
        override fun complete(id: String) {
            check(previewStates.values.none { it == NativeMmsPreviewState.DEFERRED })
            val entry = acquisitions.entries.firstOrNull { it.value.acquisitionId == id } ?: return
            entry.setValue(entry.value.copy(state = NativeMmsAcquisitionState.COMPLETE))
            completed++
        }
    }

    private fun png(): ByteArray {
        val bitmap = Bitmap.createBitmap(4, 4, Bitmap.Config.ARGB_8888)
        return ByteArrayOutputStream().use { output ->
            check(bitmap.compress(Bitmap.CompressFormat.PNG, 100, output))
            bitmap.recycle()
            output.toByteArray()
        }
    }
}
