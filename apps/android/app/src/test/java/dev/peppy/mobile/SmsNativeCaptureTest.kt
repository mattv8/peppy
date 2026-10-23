package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsNativeCaptureTest : GatewayTestBase() {
    @Test
    fun generatedCoreAdapterPersistsFullProviderEvidence() {
        val input = SmsAcquisitionInput(
            sourceGeneration = "shared-generation",
            subscriptionId = SimRoutes.routeId(1),
            providerMessageId = "42",
            providerThreadId = "7",
            incoming = true,
            senderAddress = "+15551234",
            recipients = emptyList(),
            body = "body",
            imported = false,
            sentAtMs = 1_000,
            dateSentMs = 1_001,
            creator = "messaging",
            sourceRowFingerprint = "fingerprint-42",
        )
        val core = NativeSmsCaptureCore(enrollAndUnlock())

        val acquisition = core.begin(input)

        assertEquals("fingerprint-42", acquisition.input.sourceRowFingerprint)
        assertEquals(1_001L, acquisition.input.dateSentMs)
        assertEquals("messaging", acquisition.input.creator)
        core.complete(acquisition.id)
    }

    @Test
    fun generatedSweepFindsAReusedHighWaterIdExactlyOnceAndPreservesHistoryCompletion() {
        val client = enrollAndUnlock()
        val core = NativeSmsCaptureCore(client)
        val original = providerRecord(42, "original", 1_000)
        val replacement = providerRecord(42, "replacement", 2_000)
        core.complete(core.begin(input(original)).id)
        client.planSmsScans("shared-generation", SmsCapture.PROVIDER_SCOPE, "42")
        client.ackSmsScanExhausted("shared-generation", SmsCapture.PROVIDER_SCOPE, true, "42")

        // The core defers the first sweep; a due sweep is exercised with the same frozen shape.
        assertNull(client.planSmsReconciliation("shared-generation", SmsCapture.PROVIDER_SCOPE, "42", null))
        val result = SmsCapture.reconcile(SmsReconciliationPlan("shared-generation", SmsCapture.PROVIDER_SCOPE, 0, 42, 42), core, provider(replacement))
        assertEquals(1, result.summary.captured)
        assertTrue(result.exhausted)
        val repeatedResult = SmsCapture.reconcile(SmsReconciliationPlan("shared-generation", SmsCapture.PROVIDER_SCOPE, 0, 10, 42), core, provider(replacement))
        assertEquals(0, repeatedResult.summary.captured)
        assertTrue(
            client.planSmsScans("shared-generation", SmsCapture.PROVIDER_SCOPE, "10")
                .single { it.imported }
                .historyComplete,
        )
    }

    private fun provider(record: SmsProviderRecord) = object : SmsCaptureProvider {
        override fun page(afterId: Long, limit: Int) =
            if (record.id > afterId) SmsProviderPage(listOf(record), record.id) else SmsProviderPage(emptyList(), afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
            record.takeIf { it.id <= (cursor ?: upperBound) && it.id > (floor ?: 0L) }?.let { SmsProviderPage(listOf(it), it.id) }
                ?: SmsProviderPage(emptyList(), cursor ?: upperBound)
        override fun read(id: Long) = record.takeIf { it.id == id }
    }

    private fun providerRecord(id: Long, body: String, date: Long) =
        SmsProviderRecord(id, 7, 1, "+15551234", body, date, date, 1, true, true, null, "messaging")

    private fun input(record: SmsProviderRecord) = SmsAcquisitionInput(
        "shared-generation", SimRoutes.routeId(1), record.id.toString(), record.threadId.toString(), true,
        record.address, emptyList(), record.body.orEmpty(), false, record.dateMs, record.dateSentMs, record.creator,
        SmsCapture.sourceRowFingerprint(record),
    )
}
