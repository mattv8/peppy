package dev.peppy.mobile.sms

import dev.peppy.mobile.SmsAcquisition
import dev.peppy.mobile.SmsAcquisitionInput
import dev.peppy.mobile.SmsAcquisitionState
import dev.peppy.mobile.SmsCapture
import dev.peppy.mobile.SmsCaptureCore
import dev.peppy.mobile.SmsCapturePlan
import dev.peppy.mobile.SmsCaptureProvider
import dev.peppy.mobile.SmsCompletion
import dev.peppy.mobile.diagnostics.SyncDiagnosticReason
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import dev.peppy.mobile.diagnostics.SyncTraceField
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.peppy_mobile_bindings.NativeRcsProviderRouteReason
import uniffi.peppy_mobile_bindings.NativeRcsProviderRowRoute
import uniffi.peppy_mobile_bindings.NativeRcsProviderRoute
import uniffi.peppy_mobile_bindings.MobileBindingsException

class RcsProviderRowRouterTest {
    @Test fun providerAcquires() {
        val router = RcsProviderRowRouter { route(NativeRcsProviderRowRoute.PROVIDER) }
        assertEquals(ProviderAcquisitionRoute.ACQUIRE, router.route(1, 2, 1, 3, listOf("+15550001")))
    }

    @Test fun detectorRecordedSkipsAndAdvancesSmsCheckpoint() {
        val core = SmsCore(RcsProviderRowRouter { route(NativeRcsProviderRowRoute.DETECTOR_RECORDED) })
        val summary = SmsCapture.run(SmsCapturePlan("generation", false, 0), core, SmsProvider(row()))
        assertEquals(0, core.begins)
        assertEquals(listOf("1"), core.checkpoints)
        assertTrue(summary.advanced)
    }

    @Test fun alertAcquiresAndRecordsFixedCounter() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            assertEquals(ProviderAcquisitionRoute.ACQUIRE, RcsProviderRowRouter { route(NativeRcsProviderRowRoute.ALERT) }
                .route(1, 2, 1, 3, listOf("+15550001")))
        }
        assertTrue(events.any { it.contains("${SyncTraceField.REASON.code}=${SyncDiagnosticReason.RCS_PROVIDER_ALERT}") })
    }

    @Test fun routerErrorAcquiresAndRecordsFixedCounter() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            assertEquals(ProviderAcquisitionRoute.ACQUIRE, RcsProviderRowRouter { error("no link") }
                .route(1, 2, 1, 3, listOf("+15550001")))
        }
        assertTrue(events.any { it.contains("${SyncTraceField.REASON.code}=${SyncDiagnosticReason.RCS_PROVIDER_ROUTE_ERROR}") })
    }

    @Test fun `a mirrored chat row records which side takes it`() {
        val outcomes = listOf(
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.ALERT, null) to 225L,
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.DETECTOR_RECORDED, null) to 226L,
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.PROVIDER, NativeRcsProviderRouteReason.PRE_PROOF) to 227L,
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.PROVIDER, NativeRcsProviderRouteReason.GUARD_BAND) to 228L,
            NativeRcsProviderRoute(NativeRcsProviderRowRoute.PROVIDER, NativeRcsProviderRouteReason.NOT_LINK_OWNED) to 229L,
        )
        for ((outcome, reason) in outcomes) {
            val events = mutableListOf<String>()
            SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
                RcsProviderRowRouter { outcome }.route(1, 2, 1, 3, listOf("+15550001"), restricted = true, textOnly = true)
            }
            assertTrue("$reason in $events", events.any { it.startsWith("event=${SyncTraceEvent.GM_LINK.code} ") && it.endsWith(" ${SyncTraceField.REASON.code}=$reason") })
        }
    }

    @Test fun `an ordinary provider row records no link outcome`() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, events::add) {
            RcsProviderRowRouter { NativeRcsProviderRoute(NativeRcsProviderRowRoute.PROVIDER, NativeRcsProviderRouteReason.NOT_RESTRICTED_TEXT) }
                .route(1, 2, 1, 3, listOf("+15550001"))
        }
        assertTrue("$events", events.none { it.startsWith("event=${SyncTraceEvent.GM_LINK.code} ") })
    }

    @Test fun noLinkShortCircuitCallsRouterOnlyOncePerPass() {
        var calls = 0
        val router = RcsProviderRowRouter { calls++; throw MobileBindingsException.InvalidRequest() }
        router.route(1, 2, 1, 3, listOf("+15550001"))
        router.route(2, 2, 1, 3, listOf("+15550001"))
        assertEquals(1, calls)
    }

    @Test fun nullAddressSkipsRouter() {
        var calls = 0
        val result = RcsProviderRowRouter { calls++; route(NativeRcsProviderRowRoute.DETECTOR_RECORDED) }
            .route(1, 2, 1, 3, emptyList())
        assertEquals(ProviderAcquisitionRoute.ACQUIRE, result)
        assertEquals(0, calls)
    }

    @Test fun `malformed row does not disable routing for the next valid row`() {
        var calls = 0
        val router = RcsProviderRowRouter { calls++; route(NativeRcsProviderRowRoute.DETECTOR_RECORDED) }

        assertEquals(ProviderAcquisitionRoute.ACQUIRE, router.route(-1, 2, 1, 3, listOf("+15550001")))
        assertEquals(ProviderAcquisitionRoute.SKIP, router.route(2, 2, 1, 3, listOf("+15550001")))
        assertEquals(1, calls)
    }

    @Test fun `address limit is measured in UTF-8 bytes`() {
        var calls = 0
        val address = "€".repeat(171)
        val result = RcsProviderRowRouter { calls++; route(NativeRcsProviderRowRoute.DETECTOR_RECORDED) }
            .route(1, 2, 1, 3, listOf(address))

        assertEquals(ProviderAcquisitionRoute.ACQUIRE, result)
        assertEquals(0, calls)
    }

    private fun route(value: NativeRcsProviderRowRoute) = NativeRcsProviderRoute(value, null)
    private fun row() = SmsProviderRecord(1, 2, 1, "+15550001", "body", 3, 3, 1, true, true, null, null)

    private class SmsProvider(private val record: SmsProviderRecord) : SmsCaptureProvider {
        override fun page(afterId: Long, limit: Int) = SmsProviderPage(if (record.id > afterId) listOf(record) else emptyList(), afterId)
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) = SmsProviderPage(emptyList(), cursor ?: upperBound)
        override fun read(id: Long) = record.takeIf { it.id == id }
    }

    private class SmsCore(override val providerRouter: RcsProviderRowRouter) : SmsCaptureCore {
        var begins = 0
        val checkpoints = mutableListOf<String>()
        override fun acquisitions(afterId: String?, limit: Int) = emptyList<SmsAcquisition>()
        override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
            begins++
            return SmsAcquisition("acq", input, SmsAcquisitionState.PENDING, null)
        }
        override fun complete(id: String) = SmsCompletion.CAPTURED
        override fun setState(id: String, state: SmsAcquisitionState, reason: String?) = Unit
        override fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String) { checkpoints += providerId }
    }
}
