package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.IOException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayDiagnosticsTest {
    @Test fun diagnosticsRequestCarriesParentOnlyWhenEnabled() {
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, {}) {
            assertEquals(42L, GatewayWork.createRequest(false, diagnosticParent = 42L).workSpec.input.getLong(GatewayWork.DIAGNOSTIC_PARENT, 0L))
        }
    }

    @Test fun disabledRequestDoesNotContainDiagnosticInput() {
        SyncDiagnostics.withTestConfiguration(false, { 1L }, { 1L }, {}) {
            assertEquals(-1L, GatewayWork.createRequest(false, diagnosticParent = 42L).workSpec.input.getLong(GatewayWork.DIAGNOSTIC_PARENT, -1L))
        }
    }

    @Test fun loopbackHttpRecordsOnlyCategorizedSuccessAndErrorFields() {
        val events = mutableListOf<String>()
        LoopbackHttpServer { request ->
            when (request.target) {
                "/v1/events?after=7&limit=50" -> LoopbackHttpServer.Response(200, "[]")
                else -> LoopbackHttpServer.Response(409, "bad")
            }
        }.use { server ->
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
                val http = GatewayHttp(server.origin, "secret-token")
                assertEquals(200, http.get("/v1/events?after=7&limit=50").code)
                assertEquals(409, http.postJson("/v1/attachments/private", "abc").code)
            }
        }
        val success = traceFields(events[0])
        val conflict = traceFields(events[1])
        assertEquals(SyncTraceEvent.HTTP_REQUEST.code, success.getValue("event"))
        assertEquals(GatewayHttpMethod.GET.code, success.getValue(SyncTraceField.METHOD.code.toString()))
        assertEquals(GatewayHttpRoute.EVENTS.code, success.getValue(SyncTraceField.ROUTE.code.toString()))
        assertEquals(200L, success.getValue(SyncTraceField.STATUS.code.toString()))
        assertEquals(0L, success.getValue(SyncTraceField.REQUEST_BYTES.code.toString()))
        assertEquals(2L, success.getValue(SyncTraceField.RESPONSE_BYTES.code.toString()))
        assertEquals(1L, success.getValue(SyncTraceField.SUCCESS.code.toString()))
        assertEquals(GatewayHttpMethod.POST.code, conflict.getValue(SyncTraceField.METHOD.code.toString()))
        assertEquals(GatewayHttpRoute.ATTACHMENTS.code, conflict.getValue(SyncTraceField.ROUTE.code.toString()))
        assertEquals(409L, conflict.getValue(SyncTraceField.STATUS.code.toString()))
        assertEquals(3L, conflict.getValue(SyncTraceField.REQUEST_BYTES.code.toString()))
        assertEquals(3L, conflict.getValue(SyncTraceField.RESPONSE_BYTES.code.toString()))
        assertTrue(events.none { it.contains("secret-token") || it.contains("private") || it.contains("after=7") })
    }

    @Test fun closedLoopbackTransportPreservesIOExceptionAndRecordsOneFailure() {
        val events = mutableListOf<String>()
        val server = LoopbackHttpServer { LoopbackHttpServer.Response(200, "[]") }
        val origin = server.origin
        server.close()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            try {
                GatewayHttp(origin, "token").get("/v1/events")
                fail("expected closed loopback transport failure")
            } catch (error: IOException) {
                assertFalse(error is GatewayTransportException)
            }
        }
        assertEquals(1, events.size)
        val failure = traceFields(events.single())
        assertEquals(SyncTraceEvent.HTTP_REQUEST.code, failure.getValue("event"))
        assertEquals(-1L, failure.getValue(SyncTraceField.STATUS.code.toString()))
        assertEquals(0L, failure.getValue(SyncTraceField.SUCCESS.code.toString()))
    }

    @Test fun disabledDiagnosticsDoNotReadThrowingSuppliersDuringRealHttpSuccess() {
        LoopbackHttpServer { LoopbackHttpServer.Response(200, "[]") }.use { server ->
            SyncDiagnostics.withTestConfiguration(
                false,
                { error("clock read") },
                { error("CPU read") },
                { error("sink called") },
            ) {
                assertEquals(200, GatewayHttp(server.origin, "token").get("/v1/events").code)
            }
        }
    }

    @Test fun synchronousKeepEnqueueRetainsTheFirstDiagnosticParent() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            GatewayWork.enqueue(context, diagnosticParent = 41L)
            GatewayWork.enqueue(context, diagnosticParent = 99L)
        }
        val requests = WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get()
        assertEquals(1, requests.size)
        assertTrue(requests.single().state.isFinished.not())
        val enqueues = events.map(::traceFields).filter { it["event"] == SyncTraceEvent.GATEWAY_ENQUEUE.code }
        assertEquals(2, enqueues.size)
        assertEquals(41L, enqueues[0].getValue(SyncTraceField.PARENT_ID.code.toString()))
        assertEquals(99L, enqueues[1].getValue(SyncTraceField.PARENT_ID.code.toString()))
    }

    private fun traceFields(line: String): Map<String, Long> = line.split(' ').associate { token ->
        val (key, value) = token.split('=', limit = 2)
        key to value.toLong()
    }
}
