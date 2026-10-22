package dev.peppy.mobile

import android.Manifest
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
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
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativePermitState

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

    /** Runs bounded passes like WorkManager continuations until the scanner asks for no more. */
    private fun drain(client: NativeClient): List<SmsCaptureSummary> {
        val passes = mutableListOf<SmsCaptureSummary>()
        do {
            passes += pass(client)
        } while (passes.last().more && passes.size < 20)
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
        enrollAndUnlock()
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
