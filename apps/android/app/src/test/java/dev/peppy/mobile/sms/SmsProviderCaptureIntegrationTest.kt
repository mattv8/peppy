package dev.peppy.mobile.sms

import android.Manifest
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import dev.peppy.mobile.IncomingCapture
import dev.peppy.mobile.IncomingSmsPart
import dev.peppy.mobile.SmsAcquisition
import dev.peppy.mobile.SmsAcquisitionInput
import dev.peppy.mobile.SmsAcquisitionState
import dev.peppy.mobile.SmsCapture
import dev.peppy.mobile.SmsCaptureCapacityException
import dev.peppy.mobile.SmsCaptureCore
import dev.peppy.mobile.SmsCaptureSummary
import dev.peppy.mobile.SmsCompletion
import dev.peppy.mobile.SmsReconciliationPlan
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.nextTestRecipient
import dev.peppy.mobile.mms.MmsPreferences
import dev.peppy.mobile.telephony.SimRoutes
import java.lang.reflect.Proxy
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver
import uniffi.peppy_mobile_bindings.NativeCaptured
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSmsAcquisition
import uniffi.peppy_mobile_bindings.NativeSmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeSmsProviderAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeSmsReconciliationPlan
import uniffi.peppy_mobile_bindings.NativeSmsScanPlan


/**
 * End-to-end provider capture through the real reader (a `content://sms` provider with Android's
 * column shapes), the production adapter and the generated bindings over the host SQLCipher core.
 * Rows mirror the emulator probe: `date` in milliseconds, outgoing `date_sent=0`, incoming
 * `date_sent` = SMSC time an hour away from `date`, creator package names.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsProviderCaptureIntegrationTest : GatewayTestBase() {
    private lateinit var provider: SmsProviderReaderTest.FakeSmsProvider

    @Before fun registerProvider() {
        provider = Robolectric.buildContentProvider(SmsProviderReaderTest.FakeSmsProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("sms", provider)
        WorkManagerTestInitHelper.initializeTestWorkManager(context, Configuration.Builder().setExecutor(SynchronousExecutor()).build())
    }

    private fun generation() = MmsPreferences(context).sourceGeneration

    private fun pass(client: NativeClient): SmsCaptureSummary =
        SmsNativeCapture.capture(client, AndroidSmsCaptureProvider(context), generation())

    /** Runs bounded capture passes until initial history completes, modeling later periodic work
     * when history deliberately declines an immediate continuation. */
    private fun drain(client: NativeClient): List<SmsCaptureSummary> {
        client.observeServerHighWater(client.receiveCursor())
        client.openHistoryImportGate("contacts_settled")
        val passes = mutableListOf<SmsCaptureSummary>()
        do {
            passes += pass(client)
        } while (
            (passes.last().more || !client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE).historyComplete) &&
            passes.size < 20
        )
        return passes
    }

    private fun messages(client: NativeClient) = client.listConversations().flatMap { client.messages(it.conversationId) }

    private fun row(
        id: Long,
        type: Long,
        address: String?,
        body: String?,
        date: Long?,
        dateSent: Long?,
        creator: String?,
        thread: Long = 1,
    ): Array<Any?> = arrayOf(id, thread, 1L, address, body, date, dateSent, type, 1L, 1L, -1L, creator)

    private fun reconciliationClient(
        upperBound: Long,
        state: NativeSmsAcquisitionState = NativeSmsAcquisitionState.COMPLETE,
        completions: MutableList<String?>,
    ): NativeClientInterface = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, arguments ->
        val args = arguments?.toList().orEmpty()
        when (method.name) {
            "planSmsScansWithHistoryConditions" -> emptyList<NativeSmsScanPlan>()
            "planSmsReconciliation" -> NativeSmsReconciliationPlan("reconcile", SmsCapture.PROVIDER_SCOPE, "0", upperBound.toString(), null)
            "smsAcquisitionsAfter" -> emptyList<NativeSmsAcquisition>()
            "beginSmsProviderAcquisition" -> {
                val input = args.single() as NativeSmsProviderAcquisitionInput
                NativeSmsAcquisition(input.source.providerMessageId, "conversation", input, state, null)
            }
            "completeSmsAcquisition" -> NativeCaptured("message", "conversation", false)
            "completeSmsReconciliation" -> {
                completions += args[3] as String?
                null
            }
            "setSmsProviderError" -> null
            else -> error("unexpected ${method.name}")
        }
    } as NativeClientInterface

    private fun reconciliationCapture(
        upperBound: Long,
        state: NativeSmsAcquisitionState = NativeSmsAcquisitionState.COMPLETE,
        shouldYield: () -> Boolean = { false },
        nanoTime: () -> Long = System::nanoTime,
        coreFactory: (NativeClientInterface) -> SmsCaptureCore = ::NativeSmsCaptureCore,
    ): Pair<SmsCaptureSummary, List<String?>> {
        val completions = mutableListOf<String?>()
        val summary = SmsNativeCapture.capture(
            reconciliationClient(upperBound, state, completions),
            AndroidSmsCaptureProvider(context),
            "reconcile",
            shouldYield = shouldYield,
            nanoTime = nanoTime,
            coreFactory = coreFactory,
        )
        return summary to completions
    }

    @Test fun reconciliationAdvancesMultipleEmptyPagesInOnePass() {
        (1L..125L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }

        val (summary, completions) = reconciliationCapture(125)

        assertFalse(summary.more)
        assertEquals(listOf("50", "100", "125"), completions)
    }

    @Test fun reconciliationYieldsAfterCompletingTheCurrentPage() {
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }
        var yields = false

        val (summary, completions) = reconciliationCapture(100, shouldYield = { yields = true; yields })

        assertTrue(summary.more)
        assertEquals(listOf("50"), completions)
    }

    @Test fun reconciliationStopsAtTheSharedDeadline() {
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }
        var calls = 0
        val nanoTime = { if (++calls > 51) 5_000_000_000L else 0L }

        val (summary, completions) = reconciliationCapture(100, nanoTime = nanoTime)

        assertTrue(summary.more)
        assertEquals(listOf("50"), completions)
    }

    @Test fun reconciliationStopsAfterACapturedPage() {
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }

        val (summary, completions) = reconciliationCapture(100, NativeSmsAcquisitionState.PENDING)

        assertEquals(50, summary.captured)
        assertTrue(summary.more)
        assertEquals(listOf("50"), completions)
    }

    @Test fun reconciliationDoesNotContinueAfterExhaustion() {
        (1L..49L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }

        val (summary, completions) = reconciliationCapture(49)

        assertFalse(summary.more)
        assertEquals(listOf("49"), completions)
    }

    @Test fun reconciliationCapacityBackpressureDoesNotRequestAContinuation() {
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }
        val blockedCore = object : SmsCaptureCore {
            override fun acquisitions(afterId: String?, limit: Int) = emptyList<SmsAcquisition>()
            override fun begin(input: SmsAcquisitionInput): SmsAcquisition = throw SmsCaptureCapacityException()
            override fun complete(id: String): SmsCompletion = error("unreachable")
            override fun setState(id: String, state: SmsAcquisitionState, reason: String?) = error("unreachable")
            override fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String) = error("unreachable")
        }

        val (summary, completions) = reconciliationCapture(100, coreFactory = { blockedCore })

        assertFalse(summary.more)
        assertEquals(listOf(null), completions)
    }

    @Test fun reconciliationDurablePausedAcquisitionStillAdvances() {
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "row $id", id, id, MESSAGES_APP) }

        val (summary, completions) = reconciliationCapture(100, NativeSmsAcquisitionState.PAUSED)

        assertTrue(summary.more)
        assertEquals(listOf("50"), completions)
    }

    @Test fun admittedHistoryCapturesNinetyNineRowsAndReportsTheProbeObservation() {
        grant(Manifest.permission.READ_SMS)
        (1L..100L).forEach { id ->
            provider.rows += row(id, 1, "+14155552671", "old $id", 1_600_000_000_000 + id, 1_600_000_000_000 + id, MESSAGES_APP)
        }
        var captured = 0
        var finishInput: JSONObject? = null
        val client = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, arguments ->
            val args = arguments?.toList().orEmpty()
            when {
                method.name == "planSmsScansWithHistoryConditions" -> listOf(
                    NativeSmsScanPlan(
                        "provider", SmsCapture.PROVIDER_SCOPE, true, null, true, null, "100",
                        false, false, null, 100u,
                    ),
                )
                method.name.startsWith("smsAcquisitionsAfter") -> emptyList<NativeSmsAcquisition>()
                method.name == "beginMessageHistoryCapture" -> """{"token":"claim","budget":100}"""
                method.name == "beginSmsProviderAcquisition" -> {
                    val input = args.single() as uniffi.peppy_mobile_bindings.NativeSmsProviderAcquisitionInput
                    NativeSmsAcquisition(input.source.providerMessageId, "conversation", input, NativeSmsAcquisitionState.PENDING, null)
                }
                method.name == "completeSmsAcquisition" -> {
                    captured++
                    NativeCaptured("message-$captured", "conversation", false)
                }
                method.name in setOf("setSmsScanCheckpoint", "setSmsPendingRecheckCursor", "setSmsProviderError") -> null
                method.name == "finishMessageHistoryCapture" -> {
                    finishInput = JSONObject(args.single() as String)
                    "{}"
                }
                method.name == "drainMessageHistoryCaptureResults" -> """{"published":0,"pending":0}"""
                method.name == "planSmsReconciliation" -> null
                else -> error("unexpected ${method.name}")
            }
        } as NativeClientInterface

        val summary = SmsNativeCapture.capture(client, AndroidSmsCaptureProvider(context), "provider")

        assertEquals(99, captured)
        assertEquals(99, summary.captured)
        assertEquals(100, finishInput!!.getInt("observations"))
        assertFalse(finishInput!!.getBoolean("exhausted"))
    }

    @Test fun receiverBroadcastThenProviderInboxRowIsOneMessage() {
        grant(Manifest.permission.RECEIVE_SMS, Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        assertTrue(drain(client).none { it.providerError || it.coreError }) // empty baseline
        val smsc = 1_790_879_048_000
        IncomingCapture.handle(context, listOf(IncomingSmsPart("+15557654321", "first half, ", smsc), IncomingSmsPart("+15557654321", "second half", smsc)))
        provider.rows += row(1, 1, "+15557654321", "first half, second half", smsc - 3_600_000 + 413, smsc, MESSAGES_APP)

        drain(client)

        assertEquals(listOf("first half, second half"), messages(client).map { it.body })
    }

    @Test fun providerInboxWithDifferentDateSentIsNotAssumedToMatchTheReceiverBroadcast() {
        grant(Manifest.permission.RECEIVE_SMS, Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        assertTrue(drain(client).none { it.providerError || it.coreError })
        val smsc = 1_790_879_048_000
        IncomingCapture.handle(context, listOf(IncomingSmsPart("+15557654321", "same body", smsc)))
        provider.rows += row(1, 1, "+15557654321", "same body", smsc + 413, smsc + 1, MESSAGES_APP)

        drain(client)

        assertEquals("different SMSC timestamps are distinct provider evidence", 2, messages(client).size)
    }

    @Test fun providerInboxRowThenReceiverBroadcastIsOneMessage() {
        grant(Manifest.permission.RECEIVE_SMS, Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        val smsc = 1_790_879_048_000
        provider.rows += row(1, 1, "+14155552671", "hello", smsc + 900, smsc, MESSAGES_APP)
        drain(client)
        IncomingCapture.handle(context, listOf(IncomingSmsPart("+14155552671", "hello", smsc)))
        IncomingCapture.handle(context, listOf(IncomingSmsPart("+14155552671", "hello", smsc))) // redelivery
        assertEquals(1, messages(client).size)
    }

    @Test fun peppySendAndItsPlatformRowAreOneMessageWhileNativeAppSendsAreImported() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        val recipient = nextTestRecipient()
        val commandId = queueCommand(client, SimRoutes.routeId(1), "on my way", listOf(recipient))
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(commandId).state)
        val now = System.currentTimeMillis()
        // SmsManager inserts the outbox row (Peppy creator, date_sent=0), then moves it to sent.
        provider.rows += row(1, 4, recipient, "on my way", now + 200, 0, PEPPY)
        drain(client)
        provider.rows[0][7] = 2L
        // The OEM app later sends the same text itself.
        provider.rows += row(2, 2, recipient, "on my way", now + 90_000, 0, MESSAGES_APP)
        drain(client)

        val all = messages(client)
        assertEquals(2, all.size)
        assertEquals(listOf(now + 90_000), all.mapNotNull { it.sentAtMs }) // queued send has no fabricated time
        assertEquals(0uL, client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE).pendingCount)
    }

    @Test fun firstBaselineImportsExistingRowsReadAndCapturesOnlyLaterRowsLive() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        (1L..100L).forEach { id -> provider.rows += row(id, 1, "+14155552671", "old $id", 1_600_000_000_000 + id, 1_600_000_000_000 + id, MESSAGES_APP) }

        val passes = drain(client)

        assertTrue(passes.none { it.providerError || it.coreError })
        assertEquals(100, messages(client).size)
        assertTrue(messages(client).all { it.seen }) // imported history: read, no new-message banners
        val status = client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE)
        assertTrue(status.historyComplete)
        assertFalse(status.scanning)

        provider.rows += row(101, 1, "+14155552671", "new", System.currentTimeMillis(), System.currentTimeMillis() - 1_000, MESSAGES_APP)
        drain(client)
        assertEquals(listOf("new"), messages(client).filter { !it.seen }.map { it.body })
        assertEquals("new", client.messages(client.listConversations().single().conversationId).last().body)
    }

    @Test fun closedContactsGateDefersHistoryWhileLiveCaptureContinuesThenReleasesIt() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        provider.rows += row(1, 1, "+14155552671", "history", 1_600_000_000_001, 1_600_000_000_001, MESSAGES_APP)

        val gated = pass(client)
        provider.rows += row(2, 1, "+14155552671", "live", 1_700_000_000_002, 1_700_000_000_002, MESSAGES_APP)
        val live = pass(client)

        assertEquals(0, gated.captured)
        assertEquals(1, live.captured)
        assertEquals(listOf("live"), messages(client).map { it.body })

        assertTrue(client.openHistoryImportGate("contacts_settled"))
        val replayGated = pass(client)
        assertFalse(client.initialReplayCaughtUp())
        assertEquals(0, replayGated.captured)
        assertEquals(listOf("live"), messages(client).map { it.body })
        client.observeServerHighWater(client.receiveCursor())
        val released = drain(client)

        assertTrue(released.none { it.providerError || it.coreError })
        assertEquals(listOf("history"), messages(client).filter { it.seen }.map { it.body })
        assertEquals(listOf("live"), messages(client).filter { !it.seen }.map { it.body })
    }

    @Test fun malformedRowsAreRecordedUnavailableAndHistoryContinuesPastThem() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        provider.rows += row(1, 1, null, "no sender", 1_600_000_000_001, 1_600_000_000_001, MESSAGES_APP)
        provider.rows += row(2, 2, "9".repeat(400), "too long an address", 1_600_000_000_002, 0, MESSAGES_APP)
        (3L..57L).forEach { id -> provider.rows += row(id, 2, "+14155552671", "sent $id", 1_600_000_000_000 + id, 0, MESSAGES_APP) }

        val passes = drain(client)

        assertTrue(passes.none { it.providerError || it.coreError })
        assertEquals(55, messages(client).size)
        assertEquals(2, passes.sumOf { it.unavailable })
        assertTrue(client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE).historyComplete)
        // A due reconciliation sweep over the same rows also proceeds to exhaustion.
        val sweep = SmsCapture.reconcile(SmsReconciliationPlan(generation(), SmsCapture.PROVIDER_SCOPE, 0, 57, 57), NativeSmsCaptureCore(client), AndroidSmsCaptureProvider(context))
        val rest = SmsCapture.reconcile(SmsReconciliationPlan(generation(), SmsCapture.PROVIDER_SCOPE, sweep.cursor, 57, 57), NativeSmsCaptureCore(client), AndroidSmsCaptureProvider(context))
        assertTrue(!sweep.summary.coreError && !rest.summary.coreError && rest.exhausted)
        assertEquals(55, messages(client).size)
    }

    @Test fun editedDraftAndRetriedFailedSendPublishOnceWithoutLeakingPendingRows() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        val recipient = "+14155552671"
        provider.rows += row(1, 3, recipient, "draft v1", 1_790_000_000_000, 0, MESSAGES_APP)
        drain(client)
        provider.rows[0][4] = "draft v2" // edited in place
        drain(client)
        provider.rows[0][7] = 5L // send failed
        drain(client)
        provider.rows[0][5] = 1_790_000_100_000L // retried: the app rewrites date
        provider.rows[0][7] = 2L
        drain(client)

        assertEquals(listOf("draft v2"), messages(client).map { it.body })
        assertTrue(client.smsAcquisitionsAfter(null, 100uL).isEmpty())
        assertEquals(0uL, client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE).pendingCount)
    }

    @Test fun productionRunnerCapturesAndSchedulesUploadOnlyForNewMessages() {
        // The production runner is gated by READ_SMS and the core SMS policy (which needs RECEIVE_SMS).
        grant(Manifest.permission.READ_SMS, Manifest.permission.RECEIVE_SMS)
        val client = enrollAndUnlock()
        client.observeServerHighWater(client.receiveCursor())
        client.openHistoryImportGate("contacts_settled")
        provider.rows += row(1, 1, "+14155552671", "one", 1_600_000_000_001, 1_600_000_000_001, MESSAGES_APP)
        val first = SmsNativeCapture.run(context)
        val again = SmsNativeCapture.run(context)
        assertEquals(1, first.captured)
        assertEquals(0, again.captured)
    }

    @Test fun ambiguousIdenticalPeppySendsStayTerminalThroughDueSweepsWithoutCoreRetries() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        val recipient = nextTestRecipient()
        repeat(2) {
            val commandId = queueCommand(client, SimRoutes.routeId(1), "ok", listOf(recipient))
            assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(commandId).state)
        }
        // Provider rows without a usable date (date=0, date_sent=0): either send could be either row.
        provider.rows += row(1, 2, recipient, "ok", 0, 0, PEPPY)
        provider.rows += row(2, 2, recipient, "ok", 0, 0, PEPPY)

        assertTrue(drain(client).none { it.coreError || it.providerError })
        val core = NativeSmsCaptureCore(client)
        repeat(2) {
            val sweep = SmsCapture.reconcile(SmsReconciliationPlan(generation(), SmsCapture.PROVIDER_SCOPE, 0, 2, 0), core, AndroidSmsCaptureProvider(context))
            assertTrue(sweep.exhausted)
            assertFalse(sweep.summary.coreError || sweep.summary.more)
            assertEquals(2, sweep.summary.unavailable)
        }
        val status = client.smsHistoryStatus(generation(), SmsCapture.PROVIDER_SCOPE)
        assertEquals(2uL, status.ambiguousCount)
        assertEquals(0uL, status.pendingCount)
        assertEquals(2, messages(client).size) // the two sends; nothing merged or invented
    }

    @Test fun sameHighWaterReplacedRowIsCapturedPromptlyWithoutRepeatedSweeps() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        provider.rows += row(1, 2, "+14155552671", "first", 1_790_000_000_001, 0, MESSAGES_APP)
        provider.rows += row(2, 2, "+14155552671", "second", 1_790_000_000_002, 0, MESSAGES_APP)
        drain(client)
        assertEquals(2, messages(client).size)
        // The newest row is deleted and the provider reuses its ID for the next native send.
        provider.rows.removeAt(1)
        provider.rows += row(2, 2, "+14155552671", "reused", 1_790_000_500_000, 0, MESSAGES_APP)

        drain(client)

        assertEquals(listOf("first", "second", "reused"), messages(client).map { it.body })
        // Unchanged provider state afterwards never restarts a sweep.
        repeat(3) { assertTrue(drain(client).none { it.captured > 0 || it.coreError }) }
        val anchor = SmsCapture.sourceRowFingerprint(AndroidSmsCaptureProvider(context).read(2)!!)
        assertEquals(null, client.planSmsReconciliation(generation(), SmsCapture.PROVIDER_SCOPE, "2", anchor))
    }

    @Test fun highWaterRegressionSweepsAndLiveScanResumesAtTheLowerMark() {
        grant(Manifest.permission.READ_SMS)
        val client = enrollAndUnlock()
        drain(client)
        (1L..3L).forEach { id -> provider.rows += row(id, 2, "+14155552671", "old $id", 1_790_000_000_000 + id, 0, MESSAGES_APP) }
        drain(client)
        provider.rows.clear() // provider reset; IDs restart
        provider.rows += row(1, 2, "+14155552671", "after reset", 1_790_100_000_000, 0, MESSAGES_APP)
        assertTrue(drain(client).none { it.providerError || it.coreError })
        provider.rows += row(2, 2, "+14155552671", "next", 1_790_100_100_000, 0, MESSAGES_APP)
        assertTrue(drain(client).none { it.providerError || it.coreError })

        assertEquals(listOf("old 1", "old 2", "old 3", "after reset", "next"), messages(client).map { it.body })
        repeat(2) { assertTrue(drain(client).none { it.captured > 0 || it.coreError }) }
    }

    private companion object {
        const val PEPPY = "dev.peppy.mobile"
        const val MESSAGES_APP = "com.google.android.apps.messaging"
    }
}
