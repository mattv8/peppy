package dev.peppy.mobile

import androidx.work.Configuration
import androidx.work.Data
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SyncDiagnosticsTest : GatewayTestBase() {
    @Test fun disabledDiagnosticsDoNotInvokeTheSinkOrSetATrace() {
        var sinkCalled = false
        SyncDiagnostics.withTestConfiguration(false, { 1L }, { 1L }, { sinkCalled = true }) {
            SyncDiagnostics.withTrace(7) { SyncDiagnostics.record(SyncTraceEvent.GATEWAY_START) }
            assertEquals(0L, SyncDiagnostics.currentTraceId())
        }
        assertFalse(sinkCalled)
    }

    @Test fun sinkFailureDoesNotChangeMeasuredResult() {
        var time = 10L
        val result = SyncDiagnostics.withTestConfiguration(true, { time++ }, { 2L }, { error("sink") }) {
            SyncDiagnostics.measure(SyncTraceEvent.GATEWAY_PHASE) { "unchanged" }
        }
        assertEquals("unchanged", result)
    }

    @Test fun measureRecordsDurationAndCpuAndRethrowsOriginalFailure() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, listOf(10L, 40L, 100L).iterator()::next, listOf(3L, 11L).iterator()::next, events::add) {
            val failure = IllegalStateException("original")
            try {
                SyncDiagnostics.measure(SyncTraceEvent.HTTP_REQUEST) { throw failure }
                fail("expected original failure")
            } catch (caught: IllegalStateException) {
                assertTrue(caught === failure)
            }
        }
        val event = traceFields(events.single())
        assertEquals(SyncTraceEvent.HTTP_REQUEST.code, event.getValue("event"))
        assertEquals(30L, event.getValue(SyncTraceField.DURATION_MS.code.toString()))
        assertEquals(8L, event.getValue(SyncTraceField.CPU_MS.code.toString()))
        assertEquals(0L, event.getValue(SyncTraceField.SUCCESS.code.toString()))
    }

    @Test fun nestedTraceRestoresTheOuterTrace() {
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, {}) {
            SyncDiagnostics.withTrace(3L) {
                SyncDiagnostics.withTrace(4L) { assertEquals(4L, SyncDiagnostics.currentTraceId()) }
                assertEquals(3L, SyncDiagnostics.currentTraceId())
            }
            assertEquals(0L, SyncDiagnostics.currentTraceId())
        }
    }

    @Test fun routeCategoryDoesNotExposeDynamicOrSecretPathSegments() {
        assertEquals(GatewayHttpRoute.ATTACHMENTS, GatewayHttp.routeCategory("/v1/attachments/token-should-not-appear?secret=abc"))
        assertEquals(GatewayHttpRoute.OTHER, GatewayHttp.routeCategory("/v1/unknown/password=secret"))
        assertTrue(GatewayHttp.routeCategory("/v1/events?after=123") == GatewayHttpRoute.EVENTS)
    }

    @Test fun nativeOpenNullIsUnchangedAndRecordedAsFailure() {
        context.getSharedPreferences("peppy-secure", 0).edit().clear().commit()
        NativeGateway.closeForTest()
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, listOf(10L, 30L, 40L).iterator()::next, listOf(2L, 7L).iterator()::next, events::add) {
            assertNull(NativeGateway.open(context))
        }
        val event = traceFields(events.single())
        assertEquals(SyncTraceEvent.NATIVE_OPEN.code, event.getValue("event"))
        assertEquals(1L, event.getValue(SyncTraceField.COLD.code.toString()))
        assertEquals(20L, event.getValue(SyncTraceField.DURATION_MS.code.toString()))
        assertEquals(5L, event.getValue(SyncTraceField.CPU_MS.code.toString()))
        assertEquals(0L, event.getValue(SyncTraceField.SUCCESS.code.toString()))
    }

    @Test fun nativeMeasurementRethrowsTheOriginalFailureWithExactDeltas() {
        val events = mutableListOf<String>()
        val failure = IllegalStateException("native failure")
        SyncDiagnostics.withTestConfiguration(true, listOf(20L, 50L, 80L).iterator()::next, listOf(4L, 13L).iterator()::next, events::add) {
            try {
                SyncDiagnostics.measureNativeOpen(cold = false) { throw failure }
                fail("expected native failure")
            } catch (caught: IllegalStateException) {
                assertTrue(caught === failure)
            }
        }
        val event = traceFields(events.single())
        assertEquals(30L, event.getValue(SyncTraceField.DURATION_MS.code.toString()))
        assertEquals(9L, event.getValue(SyncTraceField.CPU_MS.code.toString()))
        assertEquals(0L, event.getValue(SyncTraceField.SUCCESS.code.toString()))
    }

    @Test fun disabledMeasurementMethodsDoNotReadThrowingSuppliers() {
        SyncDiagnostics.withTestConfiguration(false, { error("clock") }, { error("CPU") }, { error("sink") }) {
            assertEquals("result", SyncDiagnostics.measure(SyncTraceEvent.HTTP_REQUEST) { "result" })
            assertEquals("phase", SyncDiagnostics.measurePhase(1L) { "phase" })
            assertEquals("native", SyncDiagnostics.measureNativeOpen(cold = true) { "native" })
        }
    }

    @Test fun gatewayWorkerRetainsSameSessionDiagnosticMetadataAndMeasuresQueueDelay() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        val input = SyncDiagnostics.withTestConfiguration(true, { 0L }, { 0L }, {}, sessionToken = 1L) {
            GatewayWork.createRequest(
                urgent = false,
                diagnosticParent = 71L,
                enqueuedMs = 0L,
                requestTrace = 72L,
                session = 1L,
            ).workSpec.input
        }
        val events = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add, sessionToken = 1L) {
            assertEquals(androidx.work.ListenableWorker.Result.success(), gatewayWorker(input).startWork().get())
        }

        val start = events.map(::traceFields).single { it["event"] == SyncTraceEvent.GATEWAY_START.code }
        assertEquals(100L, start.getValue(SyncTraceField.DELAY_MS.code.toString()))
        assertEquals(71L, start.getValue(SyncTraceField.PARENT_ID.code.toString()))
        assertEquals(72L, start.getValue(SyncTraceField.TRACE_ID.code.toString()))
        assertFalse(start.containsKey(SyncTraceField.UNAVAILABLE.code.toString()))
    }

    @Test fun gatewayWorkerRejectsMismatchedAndMissingDiagnosticMetadata() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        val mismatched = SyncDiagnostics.withTestConfiguration(true, { 0L }, { 0L }, {}, sessionToken = 1L) {
            GatewayWork.createRequest(
                urgent = false,
                diagnosticParent = 71L,
                enqueuedMs = 0L,
                requestTrace = 72L,
                session = SyncDiagnostics.currentSessionToken(),
            ).workSpec.input
        }
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 100L }, { 1L }, events::add, sessionToken = 2L) {
            assertEquals(androidx.work.ListenableWorker.Result.success(), gatewayWorker(mismatched).startWork().get())
            assertEquals(androidx.work.ListenableWorker.Result.success(), gatewayWorker(Data.EMPTY).startWork().get())
        }
        val starts = events.map(::traceFields).filter { it["event"] == SyncTraceEvent.GATEWAY_START.code }
        assertEquals(2, starts.size)
        starts.forEach { start ->
            assertEquals(1L, start.getValue(SyncTraceField.UNAVAILABLE.code.toString()))
            assertFalse(start.containsKey(SyncTraceField.DELAY_MS.code.toString()))
            assertEquals(0L, start.getValue(SyncTraceField.PARENT_ID.code.toString()))
            assertEquals(0L, start.getValue(SyncTraceField.TRACE_ID.code.toString()))
        }
    }

    private fun gatewayWorker(input: Data) = TestListenableWorkerBuilder<GatewaySyncWorker>(context).setInputData(input).build()

    private fun traceFields(line: String): Map<String, Long> = line.split(' ').associate { token ->
        val (key, value) = token.split('=', limit = 2)
        key to value.toLong()
    }
}
