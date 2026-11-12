package dev.peppy.mobile

import android.graphics.Bitmap
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.work.Data
import androidx.work.ListenableWorker
import androidx.work.workDataOf
import dev.peppy.mobile.diagnostics.SyncDiagnosticStage
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import dev.peppy.mobile.diagnostics.SyncTraceField
import dev.peppy.mobile.mms.MmsBackfillPhase
import dev.peppy.mobile.mms.MmsBackfillRecheck
import dev.peppy.mobile.mms.MmsBackfillRecheckConditions
import dev.peppy.mobile.mms.MmsBackfillRecheckOutcome
import dev.peppy.mobile.mms.MmsBackfillRecheckRequest
import dev.peppy.mobile.mms.MmsBackfillRecheckState
import dev.peppy.mobile.mms.MmsBackfillScanPlan
import dev.peppy.mobile.mms.MmsCaptureHintScheduler
import dev.peppy.mobile.mms.MmsCaptureHintScheduleSink
import dev.peppy.mobile.mms.MmsCaptureHints
import dev.peppy.mobile.mms.MmsCaptureScheduleSink
import dev.peppy.mobile.mms.MmsCaptureSummary
import dev.peppy.mobile.mms.MmsLegacyBackfillState
import dev.peppy.mobile.mms.MmsMediaPreview
import dev.peppy.mobile.mms.MmsMediaMetadata
import dev.peppy.mobile.mms.MmsMediaPreviewResult
import dev.peppy.mobile.mms.MmsMediaPreviewUnavailable
import dev.peppy.mobile.mms.MmsProviderException
import dev.peppy.mobile.mms.MmsProviderPage
import dev.peppy.mobile.mms.MmsProviderPart
import dev.peppy.mobile.mms.MmsProviderRecord
import dev.peppy.mobile.sms.RcsProviderRowRouter
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.lang.reflect.Proxy
import kotlinx.coroutines.CancellationException
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeMmsAcquisition
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPhase
import uniffi.peppy_mobile_bindings.NativeMmsBackfillScanPlan
import uniffi.peppy_mobile_bindings.NativeMmsBeginOutcome
import uniffi.peppy_mobile_bindings.NativeMmsPreviewState
import uniffi.peppy_mobile_bindings.NativeMmsProviderIdentityQuarantine
import uniffi.peppy_mobile_bindings.NativeMmsSource
import uniffi.peppy_mobile_bindings.NativeRcsProviderRowRoute
import uniffi.peppy_mobile_bindings.NativeRcsProviderRoute
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.MobileBindingsException




@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MmsCaptureOrchestrationTest {
    @Test fun mirroredOutgoingRowSkipsMmsAcquisitionWhenDetectorRecorded() {
        val core = FakeCore(providerRouter = RcsProviderRowRouter { NativeRcsProviderRoute(NativeRcsProviderRowRoute.DETECTOR_RECORDED, null) })

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(messageBox = 2, messageType = 128))), temp())

        assertEquals(0, core.inputs.size)
        assertEquals(listOf(MmsBackfillPhase.HISTORY to "1"), core.advanced)
        assertEquals(0, result.captured)
    }

    @Test fun deferredMirroredRowDefersAcquisitionAndCheckpoint() {
        var routedJson: String? = null
        val core = FakeCore(providerRouter = RcsProviderRowRouter {
            routedJson = it
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.DEFER, null)
        })

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(transactionId = "proto:abc", restricted = true, textOnly = true))), temp())

        assertEquals(0, core.inputs.size)
        assertTrue(core.advanced.isEmpty())
        assertTrue(result.more)
        assertTrue(routedJson!!.contains("\"transactionId\":\"proto:abc\""))
    }
    @Test fun recheckCompletesExistingPartUnavailableSentSmilAcquisition() {
        val smil = MmsProviderPart(10, -1, "application/smil", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val text = MmsProviderPart(11, 0, "text/plain", null, 106, "sent body", Uri.parse("content://mms/part/11"))
        val sent = record(1).copy(
            messageBox = 2,
            messageType = 128,
            sender = listOf("+15550001"),
            toRecipients = listOf("+15550002"),
            parts = listOf(smil, text),
        )
        val core = FakeCore(
            initialReason = "part_unavailable",
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", false)),
        )
        core.seed("1", incoming = false)
        val provider = FakeProvider(listOf(sent), copyErrorFor = { part ->
            if (part.providerPartId == smil.providerPartId) FileNotFoundException("Column _data not found") else null
        })

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, core.completed)
        assertFalse(provider.copiedPartIds.contains(smil.providerPartId))
        assertFalse(core.mappedProviderPartIds.contains(smil.providerPartId.toString()))
    }

    @Test fun smilOnlyMessageWithEmptyTextBecomesUnavailableWhenTheCoreRejectsIt() {
        val smil = MmsProviderPart(10, -1, "application/smil", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val core = FakeCore(completeError = MobileBindingsException.InvalidRequest())
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(smil))), copyErrorFor = { part ->
            if (part.providerPartId == smil.providerPartId) FileNotFoundException("Column _data not found") else null
        })

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(NativeMmsAcquisitionState.UNAVAILABLE, core.state("acq-1"))
        assertEquals("invalid_mms_metadata", core.reasons.last())
        assertFalse(provider.copiedPartIds.contains(smil.providerPartId))
    }

    @Test fun uppercaseSmilMediaTypeIsSkipped() {
        val smil = MmsProviderPart(10, -1, "APPLICATION/SMIL", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val text = MmsProviderPart(11, 0, "text/plain", null, 106, "body", Uri.parse("content://mms/part/11"))
        val core = FakeCore()
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(smil, text))), copyErrorFor = { part ->
            if (part.providerPartId == smil.providerPartId) FileNotFoundException("Column _data not found") else null
        })

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, result.captured)
        assertEquals(1, core.completed)
        assertFalse(provider.copiedPartIds.contains(smil.providerPartId))
    }

    @Test fun sentSmilWithInlineTextCompletesWithoutCopyingOrMappingSmil() {
        val smil = MmsProviderPart(10, -1, "application/smil", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val text = MmsProviderPart(11, 0, "text/plain", null, 106, "sent body", Uri.parse("content://mms/part/11"))
        val sent = record(1).copy(
            messageBox = 2,
            messageType = 128,
            sender = listOf("+15550001"),
            toRecipients = listOf("+15550002"),
            parts = listOf(smil, text),
        )
        val core = FakeCore()
        val provider = FakeProvider(listOf(sent), copyErrorFor = { part ->
            if (part.providerPartId == smil.providerPartId) FileNotFoundException("Column _data not found") else null
        })

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, result.captured)
        assertEquals(1, core.completed)
        assertFalse(provider.copiedPartIds.contains(smil.providerPartId))
        assertFalse(core.mappedProviderPartIds.contains(smil.providerPartId.toString()))
        assertTrue(core.preparedMediaTypes.isEmpty())
    }

    @Test fun imageMmsMapsOnlyTheImageWhenItAlsoContainsSmilAndText() {
        val smil = MmsProviderPart(10, -1, "application/smil", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val image = pngPart(11)
        val text = MmsProviderPart(12, 1, "text/plain", null, 106, "body", Uri.parse("content://mms/part/12"))
        val core = FakeCore()

        val provider = FakeProvider(
            listOf(record(1).copy(parts = listOf(smil, image, text))),
            imageBytes = png(),
            previewResult = MmsMediaPreviewResult(MmsMediaMetadata(), null, MmsMediaPreviewUnavailable.OVERSIZE_PREVIEW),
        )

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, result.captured)
        assertEquals(1, core.completed)
        assertEquals(listOf("image/png"), core.preparedMediaTypes)
        assertEquals(listOf(image.providerPartId.toString()), core.mappedProviderPartIds)
    }

    @Test fun smilDoesNotCountTowardTheBinaryAttachmentLimit() {
        val smil = MmsProviderPart(10, -1, "application/smil", null, null, "<smil/>", Uri.parse("content://mms/part/10"))
        val images = (11L..20L).map(::pngPart)
        val core = FakeCore()

        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(parts = listOf(smil) + images)), imageBytes = png()), temp())

        assertEquals(1, result.captured)
        assertEquals(1, core.completed)
        assertEquals(images.map { it.providerPartId.toString() }, core.mappedProviderPartIds)
    }

    @Test fun deterministicUnsafePreviewCompletesWithTheOriginalMapped() {
        val core = FakeCore(initialReason = "preview_deferred")
        val provider = FakeProvider(
            listOf(record(1).copy(parts = listOf(pngPart(10)))),
            imageBytes = png(),
            previewResult = MmsMediaPreviewResult(MmsMediaMetadata(20_000, 1), null, MmsMediaPreviewUnavailable.OVERSIZE_PREVIEW),
        )

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, result.captured)
        assertEquals(1, core.completed)
        assertEquals(listOf("image/png"), core.preparedMediaTypes)
        assertEquals(NativeMmsPreviewState.PERMANENTLY_UNAVAILABLE, core.previewStates("ignored")["10"])
    }

    @Test fun corruptPreviewAttemptStateDoesNotDiscardTheOriginal() {
        val directory = temp()
        File(directory, "mms-preview-interrupted-decodes").writeText("broken=\\u12zz")
        val core = FakeCore(initialReason = "preview_deferred")
        val provider = FakeProvider(
            listOf(record(1).copy(parts = listOf(pngPart(10)))),
            imageBytes = png(),
            previewResult = MmsMediaPreviewResult(MmsMediaMetadata(), null, MmsMediaPreviewUnavailable.OVERSIZE_PREVIEW),
        )

        MmsCapture.run(plan(), core, provider, directory)

        assertEquals(1, core.completed)
        assertEquals(listOf("image/png"), core.preparedMediaTypes)
    }

    @Test fun threeInterruptedPreviewDecodesQuarantineThePreviewAndCompleteTheOriginal() {
        val directory = temp()
        val core = FakeCore(
            initialReason = "preview_deferred",
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
        )
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        repeat(3) { assertTrue(PreviewDecodeAttempts(directory).begin("acq-1:10")) }

        MmsCapture.run(plan(), core, provider, directory)

        assertEquals(1, core.completed)
        assertEquals(NativeMmsPreviewState.PERMANENTLY_UNAVAILABLE, core.previewStates("ignored")["10"])
    }

    @Test fun cancellationClearsThePreviewMarkerBeforeTheNextAttempt() {
        val directory = temp()
        val core = FakeCore(
            initialReason = "preview_deferred",
            mappedParts = mapOf("10" to "attachment-10"),
            initialPreviewStates = mapOf("10" to NativeMmsPreviewState.DEFERRED),
        )
        val provider = FakeProvider(listOf(record(1).copy(parts = listOf(pngPart(10)))), imageBytes = png())
        provider.previewError = CancellationException("stopped")

        assertThrows(CancellationException::class.java) { MmsCapture.run(plan(), core, provider, directory) }
        provider.previewError = null

        MmsCapture.run(plan(), core, provider, directory)

        assertEquals(1, core.completed)
        assertEquals(NativeMmsPreviewState.READY, core.previewStates("ignored")["10"])
    }

    @Test fun interruptedPreviewDecodesQuarantineOnlyAfterThreeDurableDetections() {
        val directory = temp()
        val key = "acquisition:part"

        assertTrue(PreviewDecodeAttempts(directory).begin(key))
        assertTrue(PreviewDecodeAttempts(directory).begin(key))
        assertTrue(PreviewDecodeAttempts(directory).begin(key))
        assertFalse(PreviewDecodeAttempts(directory).begin(key))
    }

    @Test fun cancellingPreviewDecodeClearsItsInterruptedMarker() {
        val directory = temp()
        val key = "acquisition:part"
        val attempts = PreviewDecodeAttempts(directory)

        assertTrue(attempts.begin(key))
        attempts.finish(key)

        assertTrue(PreviewDecodeAttempts(directory).begin(key))
    }

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

    @Test fun diagnosticsClassifyPartCopySecurityFailuresAndRecheckOutcomes() {
        val events = mutableListOf<String>()
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)), initialReason = "preview_deferred")
        val provider = FakeProvider(
            listOf(record(1).copy(parts = listOf(pngPart(10)))),
            copyError = SecurityException(),
        )

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCapture.run(plan(), core, provider, temp())
        }

        val fields = traceFields(events.single { it.startsWith("event=${SyncTraceEvent.MMS_CAPTURE_PASS.code} ") })
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_PART_COPY.code])
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_SECURITY.code])
        assertEquals(1L, fields[SyncTraceField.RECHECK_UNCHANGED_PART_UNAVAILABLE.code])
    }

    @Test fun diagnosticsClassifyPartTextIoFailuresAndRecheckOutcomes() {
        val events = mutableListOf<String>()
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)), initialReason = "preview_deferred")
        val provider = FakeProvider(listOf(record(1)), textReadError = IOException())

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCapture.run(plan(), core, provider, temp())
        }

        val fields = traceFields(events.single { it.startsWith("event=${SyncTraceEvent.MMS_CAPTURE_PASS.code} ") })
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_PART_TEXT_READ.code])
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_IO.code])
        assertEquals(1L, fields[SyncTraceField.RECHECK_TRANSIENT_FAILURE.code])
    }

    @Test fun diagnosticsBucketProviderExceptionsBeforeStateArgumentFailures() {
        val events = mutableListOf<String>()
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)), initialReason = "preview_deferred")
        val provider = FakeProvider(
            listOf(record(1)),
            textReadError = MmsProviderException("part text unavailable"),
        )

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCapture.run(plan(), core, provider, temp())
        }

        val fields = traceFields(events.single { it.startsWith("event=${SyncTraceEvent.MMS_CAPTURE_PASS.code} ") })
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_PART_TEXT_READ.code])
        assertEquals(1L, fields[SyncTraceField.PROVIDER_FAILURE_PROVIDER.code])
        assertEquals(0L, fields[SyncTraceField.PROVIDER_FAILURE_STATE_ARGUMENT.code])
        assertEquals(0L, fields[SyncTraceField.PROVIDER_FAILURE_OTHER.code])
    }

    @Test fun nullHighWaterFailsBeforeTheCorePlansOrCheckpoints() {
        val core = FakeCore()
        val provider = FakeProvider(emptyList(), failHighWater = true)

        assertThrows(MmsProviderException::class.java) { MmsCapture.run(plan().copy(manualRecheck = true), core, provider, temp()) }

        assertEquals(0, core.migrations)
        assertTrue(core.advanced.isEmpty())
        assertEquals(null, core.manualRecheckLimit)
    }

    @Test fun corePlanDrivesLiveAndHistoryProviderScansWithoutPreferenceCheckpoints() {
        val core = FakeCore()

        // First plan freezes the baseline at 2: rows 1..2 are history only, live starts above it.
        val first = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1), record(2))), temp())

        assertEquals(1, core.migrations)
        assertEquals(listOf(MmsBackfillPhase.HISTORY to "2"), core.advanced)
        assertTrue("existing rows are first seen as history", core.inputs.first().imported)
        assertTrue("settling rows keep history open", core.finished.isEmpty())
        assertEquals("the pass's recheck phase completes the settled rows", 2, first.captured)

        val second = MmsCapture.run(plan(), core, FakeProvider((1L..3L).map(::record)), temp())

        assertEquals(listOf(MmsBackfillPhase.HISTORY to "2", MmsBackfillPhase.LIVE to "3"), core.advanced)
        assertFalse(core.inputs.first { it.source.providerMessageId == "3" }.imported)
        assertEquals(listOf("2"), core.finished)
        assertEquals(1, second.captured)
    }

    @Test fun admittedHistoryProviderPageFailureSettlesThroughProductionScanWithoutContinuation() {
        val calls = mutableListOf<String>()
        var finishInput: JSONObject? = null
        val client = Proxy.newProxyInstance(
            javaClass.classLoader,
            arrayOf(NativeClientInterface::class.java),
        ) { _, method, arguments ->
            val args = arguments?.toList().orEmpty()
            calls += method.name
            when (method.name) {
                "migrateMmsBackfillState" -> null
                "planMmsBackfillScans" -> listOf(
                    NativeMmsBackfillScanPlan(
                        NativeMmsBackfillPhase.HISTORY,
                        "generation",
                        "mms",
                        "3",
                        null,
                        false,
                        null,
                        "9",
                        false,
                        false,
                        null,
                        50u,
                    ),
                )
                "beginMessageHistoryCapture" -> """{"token":"claim","budget":50}"""
                "finishMessageHistoryCapture" -> {
                    finishInput = JSONObject(args.single() as String)
                    "{}"
                }
                "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                else -> error("unexpected ${method.name}")
            }
        } as NativeClientInterface

        val result = MmsCapture.run(
            plan(),
            nativeCore(client),
            FakeProvider(emptyList(), pageError = MmsProviderException("provider unavailable")),
            temp(),
        )

        assertEquals(
            listOf(
                "migrateMmsBackfillState",
                "planMmsBackfillScans",
                "beginMessageHistoryCapture",
                "finishMessageHistoryCapture",
                "drainMessageHistoryCaptureResults",
            ),
            calls,
        )
        assertEquals("claim", finishInput!!.getString("token"))
        assertEquals(0, finishInput!!.getInt("observations"))
        assertFalse(finishInput!!.getBoolean("exhausted"))
        assertEquals("provider_error", finishInput!!.getString("reason"))
        assertFalse(result.more)
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

    @Test fun descendingHistoryUsesStrictCursorAcrossPagesAndExhaustsWithoutRepeatingBoundary() {
        val core = FakeCore(
            initialLiveCursor = "55",
            historyUpperBound = "55",
            historyDescending = true,
            initialState = NativeMmsAcquisitionState.COMPLETE,
        )
        val provider = FakeProvider((1L..55L).map(::record))

        val first = MmsCapture.run(plan(), core, provider, temp())
        val second = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(listOf(null, 6L), provider.descendingPageCursors)
        assertEquals(1, provider.descendingPageIds.flatten().count { it == 6L })
        assertEquals(listOf(MmsBackfillPhase.HISTORY to "6", MmsBackfillPhase.HISTORY to "1"), core.advanced)
        assertTrue(first.more)
        assertFalse(second.more)
        assertEquals(listOf("55"), core.finished)
    }

    @Test fun descendingHistoryRecordsMissingSubscriptionsAsUnavailableAndContinues() {
        val core = FakeCore(historyUpperBound = "5", historyDescending = true, historyFloor = "2")
        val records = (3L..5L).map(::record).toMutableList()
        records[0] = records[0].copy(subscriptionId = null)

        MmsCapture.run(plan(), core, FakeProvider(records), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.HISTORY to "3"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "3" && it.source.subscriptionId == "android-mms-unknown-subscription" })
    }

    @Test fun revokedAccessDuringDescendingRescanPageDoesNotCreateAnAcquisitionOrCommit() {
        var access = dev.peppy.mobile.mms.RestrictedMessageAccess.ALLOWED
        val core = FakeCore(historyUpperBound = "5", providerRescanToken = "token")
        val provider = FakeProvider((1L..5L).map(::record)).also {
            it.onDescendingPage = { access = dev.peppy.mobile.mms.RestrictedMessageAccess.NOT_ALLOWED }
        }

        val result = MmsCapture.run(
            plan(), core, provider, temp(),
            restrictedAccess = { access },
        )

        assertTrue(core.inputs.isEmpty())
        assertTrue(core.rescanCommits.isEmpty())
        assertFalse(result.more)
    }

    @Test fun revokedAccessDuringStableReadDoesNotPersistProviderRowMissingOrCommit() {
        var access = dev.peppy.mobile.mms.RestrictedMessageAccess.ALLOWED
        val core = FakeCore(historyUpperBound = "1", providerRescanToken = "token", initialReason = "preview_deferred")
        val provider = FakeProvider(listOf(record(1))).also {
            it.onRead = { access = dev.peppy.mobile.mms.RestrictedMessageAccess.NOT_ALLOWED }
        }

        MmsCapture.run(plan(), core, provider, temp(), restrictedAccess = { access })

        assertTrue(core.reasons.none { it == "provider_row_missing" })
        assertTrue(core.rescanCommits.isEmpty())
    }

    @Test fun revokedAccessDuringPartReadIsNotClassifiedAsTextUnavailable() {
        var access = dev.peppy.mobile.mms.RestrictedMessageAccess.ALLOWED
        val core = FakeCore(historyUpperBound = "1", providerRescanToken = "token", initialReason = "preview_deferred")
        val provider = FakeProvider(listOf(record(1))).also {
            it.onReadPartText = { access = dev.peppy.mobile.mms.RestrictedMessageAccess.NOT_ALLOWED }
        }

        MmsCapture.run(plan(), core, provider, temp(), restrictedAccess = { access })

        assertTrue(core.reasons.none { it == "text_unavailable" })
        assertTrue(core.rescanCommits.isEmpty())
    }

    @Test fun providerRescanReadsDescendingAsImportedWithoutSettlingOrdinaryHistory() {
        val core = FakeCore(historyUpperBound = "1", providerRescanToken = "token", initialReason = "preview_deferred")
        val provider = FakeProvider(listOf(record(1)))

        MmsCapture.run(plan(), core, provider, temp())

        assertEquals(1, provider.descendingPageCount)
        assertTrue(core.inputs.all { it.imported })
        assertEquals(listOf("token:null:1:true"), core.rescanCommits)
        assertTrue(core.finished.isEmpty())
    }

    @Test fun failedProviderRescanPageDoesNotBlockHistoryOrCommitTheRescan() {
        val core = FakeCore(
            historyUpperBound = "1",
            providerRescanToken = "token",
            providerRescanAlongsideOrdinaryPlans = true,
        )
        val provider = FakeProvider(listOf(record(1)), descendingPageError = MmsProviderException("unavailable"))

        MmsCapture.run(plan(), core, provider, temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.HISTORY to "1"))
        assertTrue(core.rescanCommits.isEmpty())
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

    @Test fun partialLivePageWithMissingSubscriptionIsDurablyUnavailableAndAdvances() {
        val core = FakeCore(historyUpperBound = "0")
        val result = MmsCapture.run(plan(), core, FakeProvider(listOf(record(1).copy(subscriptionId = null))), temp())

        assertFalse(result.more)
        assertEquals(1, result.unavailable)
        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "1"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "1" && it.source.subscriptionId == "android-mms-unknown-subscription" })
    }

    @Test fun livePageAdvancesPastAnInvalidSimOnRowTwoWithoutSkippingLaterRows() {
        val core = FakeCore(historyUpperBound = "0")
        val records = (1L..60L).map(::record).toMutableList()
        records[1] = records[1].copy(subscriptionId = null)

        MmsCapture.run(plan(), core, FakeProvider(records), temp())

        assertTrue(core.advanced.contains(MmsBackfillPhase.LIVE to "50"))
        assertTrue(core.inputs.any { it.source.providerMessageId == "2" && it.source.subscriptionId == "android-mms-unknown-subscription" })
        assertTrue(core.inputs.any { it.source.providerMessageId == "50" })
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

    @Test fun recheckProviderReadCancellationPropagatesWithoutRecordingFailure() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))
        val provider = FakeProvider(listOf(record(1)), readError = CancellationException("stopped"))

        assertThrows(CancellationException::class.java) { MmsCapture.run(plan(), core, provider, temp()) }

        assertTrue(core.recheckOutcomes.isEmpty())
        assertTrue(core.inputs.isEmpty())
    }

    @Test fun recheckProcessCancellationPropagatesWithoutRecordingFailure() {
        val core = FakeCore(
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)),
            initialReason = "preview_deferred",
        )
        val provider = FakeProvider(
            listOf(record(1).copy(parts = listOf(pngPart(10)))),
            copyError = CancellationException("stopped"),
        )

        assertThrows(CancellationException::class.java) { MmsCapture.run(plan(), core, provider, temp()) }

        assertTrue(core.recheckOutcomes.isEmpty())
        assertTrue(core.reasons.isEmpty())
    }

    @Test fun recheckProviderReadFailureRecordsTransientOutcome() {
        val core = FakeCore(recheckRequests = listOf(MmsBackfillRecheckRequest("1", true)))
        val provider = FakeProvider(listOf(record(1)), readError = MmsProviderException("unavailable"))

        val result = MmsCapture.run(plan(), core, provider, temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.TRANSIENT_FAILURE), core.recheckOutcomes)
        assertFalse(result.more)
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
        val core = FakeCore(historyUpperBound = "0", quarantinedProviderIds = setOf("1"))
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

    @Test fun pendingRecheckResolvedToSameAcquisitionIsVerified() {
        val core = FakeCore(
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", true, acquisitionId = "acq-1")),
            initialState = NativeMmsAcquisitionState.COMPLETE,
        )

        MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.VERIFIED), core.recheckOutcomes)
    }

    @Test fun pendingRecheckResolvedToDifferentAcquisitionIsTransient() {
        val core = FakeCore(
            recheckRequests = listOf(MmsBackfillRecheckRequest("1", true, acquisitionId = "acq-a")),
            initialState = NativeMmsAcquisitionState.COMPLETE,
            resolvedAcquisitionId = "acq-b",
        )

        MmsCapture.run(plan(), core, FakeProvider(listOf(record(1))), temp())

        assertEquals(listOf(MmsBackfillRecheckOutcome.TRANSIENT_FAILURE), core.recheckOutcomes)
    }

    @Test fun twoHintsBeforeAPassScheduleOneDebouncedWork() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestHint(sink)
        scheduler.requestHint(sink)

        assertEquals(listOf(androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true), scheduled)
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
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun hintReplacesQueuedManualReplacement() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.replace(sink)
        scheduler.requestHint(sink)

        assertEquals(
            listOf(
                androidx.work.ExistingWorkPolicy.REPLACE to false,
                androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true,
            ),
            scheduled,
        )
    }

    @Test fun hintReplacesQueuedNormalPass() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.requestNormal(sink)
        scheduler.requestHint(sink)

        assertEquals(listOf(androidx.work.ExistingWorkPolicy.KEEP to true, androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true), scheduled)
    }

    @Test fun hintReplacesFencedPass() {
        val scheduler = MmsCaptureHintScheduler()
        val scheduled = mutableListOf<Pair<androidx.work.ExistingWorkPolicy, Boolean>>()
        val sink = MmsCaptureScheduleSink { policy, delayed -> scheduled += policy to delayed }

        scheduler.beginPass(managedWork = true)
        scheduler.finishPass(managedWork = true)
        scheduler.requestHint(sink)

        assertEquals(listOf(androidx.work.ExistingWorkPolicy.APPEND_OR_REPLACE to true), scheduled)
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
            ),
            scheduled,
        )
    }

    @Test fun managedDataRecognizesHeadOneTimeRequestsButNotEmptyPeriodicRequests() {
        assertTrue(MmsCaptureWork.isManagedData(workDataOf(MmsCaptureWork.MANUAL_RECHECK to false)))
        assertFalse(MmsCaptureWork.isManagedData(Data.EMPTY))
    }

    @Test fun hintFailureRestoresDurableHintAndReturnsSuccess() {
        val context = hintContext()
        MmsCaptureHints.record(context)
        val events = java.util.concurrent.CopyOnWriteArrayList<String>()

        val result = SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            MmsCaptureWork.runWorkerPass(context, workDataOf(MmsCaptureWork.HINT_WORK to true)) { _, _ ->
                throw IOException("provider unavailable")
            }
        }

        assertEquals(ListenableWorker.Result.success(), result)
        assertTrue(MmsCaptureHints.consumeProviderChange(context))
        assertEquals(1, events.count { event ->
            event.startsWith("event=${SyncTraceEvent.CAPTURE_PROGRESS.code} ") &&
                traceFields(event)[SyncTraceField.STAGE.code] == SyncDiagnosticStage.MMS &&
                traceFields(event)[SyncTraceField.COUNT.code] == 4L
        })
        assertTrue(events.none { it.contains("provider unavailable") })
    }

    @Test fun hintCancellationPropagatesAndRestoresDurableHint() {
        val context = hintContext()
        MmsCaptureHints.record(context)

        try {
            MmsCaptureWork.runWorkerPass(context, workDataOf(MmsCaptureWork.HINT_WORK to true)) { _, _ ->
                throw CancellationException("stopped")
            }
            fail("expected cancellation")
        } catch (_: CancellationException) {
            assertTrue(MmsCaptureHints.consumeProviderChange(context))
        }
    }

    @Test fun failedHintEnqueueReleasesOnlyItsReservation() {
        val scheduler = MmsCaptureHintScheduler()
        val tokens = mutableListOf<Long>()
        lateinit var failFirst: (Long) -> Unit
        val sink = MmsCaptureHintScheduleSink { token, onFailure ->
            tokens += token
            if (tokens.size == 1) failFirst = onFailure
        }

        scheduler.requestHint(sink)
        scheduler.requestHint(sink)
        failFirst(tokens.single())
        scheduler.requestHint(sink)

        assertEquals(2, tokens.size)
        assertTrue(tokens[1] > tokens[0])
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

    @Test fun deferredBarrierPreservesManualAndProviderIntentWithoutHotContinuation() {
        val context = hintContext()

        MmsCaptureWork.deferUntilProducerReady(context, manualRecheck = true, providerChanged = false)
        MmsCaptureWork.deferUntilProducerReady(context, manualRecheck = false, providerChanged = true)
        val deferred = MmsCaptureWork.consumeDeferredCapture(context)

        assertEquals(MmsCaptureWork.DeferredCapture(manualRecheck = true, providerChanged = true), deferred)
        assertEquals(null, MmsCaptureWork.consumeDeferredCapture(context))
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

    private fun nativeCore(client: NativeClientInterface): MmsCaptureCore {
        val constructor = Class.forName("dev.peppy.mobile.NativeCore")
            .getDeclaredConstructor(NativeClientInterface::class.java)
            .apply { isAccessible = true }
        return constructor.newInstance(client) as MmsCaptureCore
    }
    private fun temp() = kotlin.io.path.createTempDirectory("mms-capture-test").toFile()
    private fun hintContext(): android.content.Context = ApplicationProvider.getApplicationContext<android.content.Context>().also {
        it.getSharedPreferences(MmsCaptureHints.PREFERENCES, android.content.Context.MODE_PRIVATE).edit().clear().commit()
    }
    private fun record(id: Long) = MmsProviderRecord(
        id, 4, 7, 1, 1_000, null, "subject", listOf("+15550001"), emptyList(), listOf("+15550002"), emptyList(),
        132, null, listOf(MmsProviderPart(id * 10, 0, "text/plain", null, 106, "body", Uri.parse("content://mms/part/$id"))), true, null,
    )
    private fun traceFields(line: String): Map<Long, Long> = line.split(' ').mapNotNull { token ->
        token.split('=', limit = 2).takeIf { it.size == 2 }?.let { (key, value) -> key.toLongOrNull()?.let { it to value.toLong() } }
    }.toMap()

    private class FakeProvider(
        private val records: List<MmsProviderRecord>,
        private val imageBytes: ByteArray? = null,
        private val failCopy: Boolean = false,
        private val copyError: Exception? = null,
        private val copyErrorFor: (MmsProviderPart) -> Exception? = { null },
        private val textReadError: Exception? = null,
        private val previewLimit: Int = MmsMediaPreview.MAX_JPEG_BYTES,
        private val previewResult: MmsMediaPreviewResult? = null,
        private val failHighWater: Boolean = false,
        private val descendingPageError: Exception? = null,
        private val pageError: Exception? = null,
        private val readError: Exception? = null,
    ) : MmsCaptureProvider {
        var copyCount = 0
        val copiedPartIds = mutableListOf<Long>()
        var descendingPageCount = 0
        val descendingPageCursors = mutableListOf<Long?>()
        val descendingPageIds = mutableListOf<List<Long>>()
        var previewError: Exception? = null
        var onDescendingPage: (() -> Unit)? = null
        var onRead: (() -> Unit)? = null
        var onReadPartText: (() -> Unit)? = null
        override fun inspectMedia(file: File, contentType: String?): MmsMediaPreviewResult {
            previewError?.let { throw it }
            return previewResult ?: MmsMediaPreview(previewLimit).inspect(file, contentType)
        }
        override fun highWater(): Long {
            if (failHighWater) throw MmsProviderException("MMS provider returned no high-water cursor")
            return records.maxOfOrNull { it.id } ?: 0
        }
        override fun page(afterId: Long, limit: Int) = pageError?.let { throw it }
            ?: MmsProviderPage(records.filter { it.id > afterId }.take(limit), afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): MmsProviderPage {
            descendingPageCount++
            descendingPageCursors += cursor
            descendingPageError?.let { throw it }
            val page = records.filter { record ->
                record.id <= upperBound && record.id > (floor ?: 0L) && (cursor == null || record.id < cursor)
            }.sortedByDescending { it.id }.take(limit)
            descendingPageIds += page.map { it.id }
            val ceiling = cursor ?: upperBound
            return MmsProviderPage(page, page.lastOrNull()?.id ?: ceiling).also { onDescendingPage?.invoke() }
        }
        override fun read(id: Long) = readError?.let { throw it } ?: records.firstOrNull { it.id == id }.also { onRead?.invoke() }
        override fun readPartText(part: MmsProviderPart, byteLimit: Int): String? {
            textReadError?.let { throw it }
            return part.text.also { onReadPartText?.invoke() }
        }
        override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long): Long {
            copyCount++
            copiedPartIds += part.providerPartId
            copyError?.let { throw it }
            copyErrorFor(part)?.let { throw it }
            if (failCopy) error("provider read failed")
            val bytes = imageBytes ?: byteArrayOf()
            destination.writeBytes(bytes)
            return bytes.size.toLong()
        }
    }

    /**
     * Plans like `plan_mms_backfill_scans`: the first plan freezes the history baseline at the
     * observed maximum (unless [historyUpperBound] supplies an existing one) and starts live just
     * above it, so live and history never overlap. Each pass also gets a recheck phase over the
     * pending acquisitions. The completed-row recheck partition and its cursors are not modelled.
     */
    private class FakeCore(
        initialLiveCursor: String? = null,
        historyUpperBound: String? = null,
        private val historyDescending: Boolean = false,
        private val historyFloor: String? = null,
        private val mappedParts: Map<String, String> = emptyMap(),
        private val recheckRequests: List<MmsBackfillRecheckRequest> = emptyList(),
        private val quarantinedProviderIds: Set<String> = emptySet(),
        private val failBeginProviderIds: Set<String> = emptySet(),
        private val failManualRecheck: Boolean = false,
        private val providerRescanToken: String? = null,
        private val providerRescanAlongsideOrdinaryPlans: Boolean = false,
        initialPreviewStates: Map<String, NativeMmsPreviewState> = emptyMap(),
        var pendingBytes: Long = 0,
        private val initialReason: String = "settling",
        private val initialState: NativeMmsAcquisitionState = NativeMmsAcquisitionState.PENDING,
        private val completeError: Exception? = null,
        private val resolvedAcquisitionId: String? = null,
        override val providerRouter: RcsProviderRowRouter = RcsProviderRowRouter.acquireAll(),
    ) : MmsCaptureCore {
        var migrations = 0
        val reasons = mutableListOf<String?>()
        val advanced = mutableListOf<Pair<MmsBackfillPhase, String>>()
        val finished = mutableListOf<String>()
        val rescanCommits = mutableListOf<String>()
        val inputs = mutableListOf<NativeMmsAcquisitionInput>()
        var completed = 0
        var manualRecheckLimit: Int? = null
        val preparedMediaTypes = mutableListOf<String>()
        val mappedProviderPartIds = mutableListOf<String>()
        private val previewStates = initialPreviewStates.toMutableMap()
        private val acquisitions = mutableMapOf<String, NativeMmsAcquisition>()
        private var liveCursor = initialLiveCursor
        private var baseline = historyUpperBound
        private var historyCursor: String? = null
        private var historyComplete = false
        override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) { migrations++ }
        override fun plans(generation: String, scope: String, observedMaxId: String): List<MmsBackfillScanPlan> {
            val bound = baseline ?: observedMaxId.also { baseline = it }
            if (liveCursor == null) liveCursor = bound
            providerRescanToken?.let {
                val rescan = MmsBackfillScanPlan(MmsBackfillPhase.PROVIDER_RESCAN, null, bound, false, false, 50, true, null, it)
                if (!providerRescanAlongsideOrdinaryPlans) return listOf(rescan)
                return listOf(
                    MmsBackfillScanPlan(MmsBackfillPhase.LIVE, liveCursor, bound, historyComplete, false, 50),
                    rescan,
                    MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, historyCursor, bound, historyComplete, false, 50, historyDescending, historyFloor),
                    MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, historyComplete, false, 50),
                )
            }
            if (recheckRequests.isNotEmpty()) return listOf(MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, false, false, 50))
            return listOf(
                MmsBackfillScanPlan(MmsBackfillPhase.LIVE, liveCursor, bound, historyComplete, false, 50),
                MmsBackfillScanPlan(MmsBackfillPhase.HISTORY, historyCursor, bound, historyComplete, false, 50, historyDescending, historyFloor),
                MmsBackfillScanPlan(MmsBackfillPhase.RECHECK, null, null, historyComplete, false, 50),
            )
        }
        override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) {
            advanced += phase to providerId
            when (phase) {
                MmsBackfillPhase.LIVE -> liveCursor = providerId
                MmsBackfillPhase.HISTORY -> historyCursor = providerId
                MmsBackfillPhase.PROVIDER_RESCAN -> error("provider rescan uses its tokened commit")
                MmsBackfillPhase.RECHECK -> error("recheck has no page checkpoint")
            }
        }
        override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean): Boolean {
            if (!exhausted) return false
            finished += bound
            historyComplete = bound == baseline && pendingAcquisitions().isEmpty()
            return historyComplete
        }
        override fun commitProviderRescanPage(
            generation: String,
            scope: String,
            token: String,
            expectedCursor: String?,
            lastDurableId: String?,
            exhausted: Boolean,
        ): Boolean {
            rescanCommits += "$token:$expectedCursor:$lastDurableId:$exhausted"
            return true
        }
        val dueConditions = mutableListOf<MmsBackfillRecheckConditions>()
        val recheckOutcomes = mutableListOf<MmsBackfillRecheckOutcome>()
        override fun dueRechecks(generation: String, scope: String, nowMs: Long, conditions: MmsBackfillRecheckConditions): MmsBackfillRecheck {
            dueConditions += conditions
            val requests = recheckRequests.ifEmpty {
                pendingAcquisitions().map { MmsBackfillRecheckRequest(it.input.source.providerMessageId, it.input.incoming) }
            }
            return MmsBackfillRecheck(requests, 50, continuationDue = requests.isNotEmpty())
        }
        override fun recordRecheckOutcome(acquisitionId: String, nowMs: Long, outcome: MmsBackfillRecheckOutcome, conditions: MmsBackfillRecheckConditions) {
            recheckOutcomes += outcome
        }
        /** Quarantined identities never become acquisitions in the core, although this fake records them. */
        private fun pendingAcquisitions() = acquisitions
            .filterKeys { it !in quarantinedProviderIds }
            .values
            .filter { it.state == NativeMmsAcquisitionState.PENDING }
        fun seed(providerMessageId: String, incoming: Boolean) {
            val input = NativeMmsAcquisitionInput(
                NativeMmsSource("generation", "route", providerMessageId, null),
                incoming,
                null,
                emptyList(),
                null,
                "",
                false,
                0,
                null,
            )
            acquisitions[providerMessageId] = NativeMmsAcquisition(
                "acq-$providerMessageId",
                "conversation",
                input,
                initialState,
                initialReason,
                emptyList(),
            )
        }
        fun state(acquisitionId: String) = acquisitions.values.first { it.acquisitionId == acquisitionId }.state
        override fun begin(input: NativeMmsAcquisitionInput): NativeMmsBeginOutcome {
            inputs += input
            if (input.source.providerMessageId in failBeginProviderIds) error("core unavailable")
            val acquisition = acquisitions.getOrPut(input.source.providerMessageId) {
                NativeMmsAcquisition("acq-${input.source.providerMessageId}", "conversation", input, initialState, initialReason, emptyList())
            }
            return if (input.source.providerMessageId in quarantinedProviderIds) {
                NativeMmsBeginOutcome.Quarantined(NativeMmsProviderIdentityQuarantine(input.source, input.incoming, "ambiguous"))
            } else {
                NativeMmsBeginOutcome.Acquired(resolvedAcquisitionId?.let { acquisition.copy(acquisitionId = it) } ?: acquisition)
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
        override fun mapPart(id: String, providerPartId: String, attachmentId: String) {
            mappedProviderPartIds += providerPartId
        }
        override fun setPartMetadata(id: String, providerPartId: String, width: Int?, height: Int?, durationMs: Long?, thumbnailAttachmentId: String?) = Unit
        override fun setPartPreviewState(id: String, providerPartId: String, state: NativeMmsPreviewState) {
            previewStates[providerPartId] = state
        }
        override fun discardAttachment(id: String) = Unit
        override fun complete(id: String) {
            completeError?.let { throw it }
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
