package dev.peppy.mobile

import android.net.Uri
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeMmsAcquisition
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
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
        val core = FakeCore(liveCursor = "100", historyUpperBound = "100")

        MmsCapture.run(plan(), core, FakeProvider(listOf(record(101))), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "101"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "101" && !it.imported })
    }

    private fun plan() = MmsCapturePlan("generation", null, false, null, {}, {}, null, {}, false)
    private fun temp() = kotlin.io.path.createTempDirectory("mms-capture-test").toFile()
    private fun record(id: Long) = MmsProviderRecord(
        id, 4, 7, 1, 1_000, null, "subject", listOf("+15550001"), emptyList(), listOf("+15550002"), emptyList(),
        132, null, listOf(MmsProviderPart(id * 10, 0, "text/plain", null, 106, "body", Uri.parse("content://mms/part/$id"))), true, null,
    )

    private class FakeProvider(private val records: List<MmsProviderRecord>) : MmsCaptureProvider {
        override fun highWater() = records.maxOfOrNull { it.id } ?: 0
        override fun page(afterId: Long, limit: Int) = MmsProviderPage(records.filter { it.id > afterId }.take(limit), afterId)
        override fun read(id: Long) = records.firstOrNull { it.id == id }
        override fun readPartText(part: MmsProviderPart, byteLimit: Int) = part.text
        override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long) = 0L
    }

    private class FakeCore(
        private val liveCursor: String = "0",
        private val historyUpperBound: String? = null,
    ) : MmsCaptureCore {
        var migrations = 0
        val advanced = mutableListOf<Pair<MmsBackfillPhase, String>>()
        val finished = mutableListOf<String>()
        val inputs = mutableListOf<NativeMmsAcquisitionInput>()
        private val acquisitions = mutableMapOf<String, NativeMmsAcquisition>()
        override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) { migrations++ }
        override fun plans(generation: String, scope: String, observedMaxId: String) = listOf(
            MmsBackfillScanPlan(MmsBackfillPhase.LIVE, liveCursor, historyUpperBound ?: observedMaxId, false, false, 50),
            MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, "0", historyUpperBound ?: observedMaxId, false, false, 50),
        )
        override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) { advanced += phase to providerId }
        override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean { if (exhausted) finished += bound; return exhausted }
        override fun rechecks(generation: String, scope: String) = MmsBackfillRecheck(emptyList(), 50)
        override fun acquisitions(limit: Int) = acquisitions.values.toList()
        override fun begin(input: NativeMmsAcquisitionInput): NativeMmsAcquisition {
            inputs += input
            return acquisitions.getOrPut(input.source.providerMessageId) {
                NativeMmsAcquisition("acq-${input.source.providerMessageId}", "conversation", input, NativeMmsAcquisitionState.PENDING, "settling", emptyList())
            }
        }
        override fun setState(id: String, state: NativeMmsAcquisitionState, reason: String?) {
            val entry = acquisitions.entries.firstOrNull { it.value.acquisitionId == id } ?: return
            entry.setValue(entry.value.copy(state = state, reason = reason))
        }
        override fun mappedParts(id: String) = emptyMap<String, String>()
        override fun partMetadataComplete(id: String) = emptySet<String>()
        override fun pendingMediaBytes() = 0L
        override fun prepareAttachment(path: String, mediaType: String, name: String) = "attachment"
        override fun mapPart(id: String, providerPartId: String, attachmentId: String) = Unit
        override fun setPartMetadata(id: String, providerPartId: String, width: Int?, height: Int?, durationMs: Long?, thumbnailAttachmentId: String?) = Unit
        override fun discardAttachment(id: String) = Unit
        override fun complete(id: String) = Unit
    }
}
