package dev.peppy.mobile

import android.graphics.Bitmap
import android.net.Uri
import java.io.ByteArrayOutputStream
import java.io.File
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
        MmsCapture.run(plan(), core, provider, temp())
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

    private fun plan() = MmsCapturePlan("generation", null, false, null, null, false)
    private fun temp() = kotlin.io.path.createTempDirectory("mms-capture-test").toFile()
    private fun record(id: Long) = MmsProviderRecord(
        id, 4, 7, 1, 1_000, null, "subject", listOf("+15550001"), emptyList(), listOf("+15550002"), emptyList(),
        132, null, listOf(MmsProviderPart(id * 10, 0, "text/plain", null, 106, "body", Uri.parse("content://mms/part/$id"))), true, null,
    )

    private class FakeProvider(
        private val records: List<MmsProviderRecord>,
        private val imageBytes: ByteArray? = null,
        private val failCopy: Boolean = false,
    ) : MmsCaptureProvider {
        var copyCount = 0
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
    ) : MmsCaptureCore {
        var migrations = 0
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
        override fun rechecks(generation: String, scope: String) = MmsBackfillRecheck(recheckRequests, 50)
        override fun begin(input: NativeMmsAcquisitionInput): NativeMmsBeginOutcome {
            inputs += input
            if (input.source.providerMessageId in failBeginProviderIds) error("core unavailable")
            val acquisition = acquisitions.getOrPut(input.source.providerMessageId) {
                NativeMmsAcquisition("acq-${input.source.providerMessageId}", "conversation", input, NativeMmsAcquisitionState.PENDING, "settling", emptyList())
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
