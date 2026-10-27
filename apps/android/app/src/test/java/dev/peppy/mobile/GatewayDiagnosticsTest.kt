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
    @Test fun cleanupReportsOnlyBoundedNumericCountsAndHonorsDiagnosticsOptOut() {
        val traces = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, traces::add) {
            BackgroundDiagnostics.mmsCleanup(3, 2, 1)
        }
        assertEquals(1, traces.size)
        val fields = traces.single().split(' ').associate { token -> token.substringBefore('=') to token.substringAfter('=') }
        assertEquals("20", fields[SyncTraceField.PHASE.code.toString()])
        assertEquals("3", fields[SyncTraceField.COUNT.code.toString()])
        assertEquals("2", fields[SyncTraceField.PROCESSED.code.toString()])
        assertEquals("1", fields[SyncTraceField.PENDING.code.toString()])
        SyncDiagnostics.withTestConfiguration(false, { error("clock") }, { error("cpu") }, traces::add) {
            BackgroundDiagnostics.mmsCleanup(3, 2, 1)
        }
        assertEquals(1, traces.size)
    }
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

    @Test fun backgroundProgressUsesOnlyNumericCountersWhenEnabled() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            BackgroundDiagnostics.gateway(received = 3, snapshotReceived = 5, progressed = 2, uploaded = 1, snapshotRemaining = 4, readiness = null)
        }

        val fields = traceFields(events.single())
        assertEquals(SyncTraceEvent.GATEWAY_PROGRESS.code, fields.getValue("event"))
        assertEquals(3L, fields.getValue(SyncTraceField.ROWS.code.toString()))
        assertEquals(5L, fields.getValue(SyncTraceField.PENDING.code.toString()))
        assertEquals(2L, fields.getValue(SyncTraceField.PROCESSED.code.toString()))
        assertEquals(1L, fields.getValue(SyncTraceField.COUNT.code.toString()))
        assertEquals(4L, fields.getValue(SyncTraceField.REMAINING.code.toString()))
    }

    @Test fun disabledBackgroundProgressDoesNotReadDiagnosticsSuppliers() {
        SyncDiagnostics.withTestConfiguration(false, { error("clock read") }, { error("CPU read") }, { error("sink called") }) {
            BackgroundDiagnostics.gateway(received = 3, snapshotReceived = 5, progressed = 2, uploaded = 1, snapshotRemaining = 4, readiness = null)
        }
    }

    @Test fun gatewayReadinessFixtureRecordsCoreReadyAndBackfillState() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            val readiness = BackgroundDiagnostics.gatewayReadiness("""{"contacts_ready":true,"state":"ready"}""")
            BackgroundDiagnostics.gateway(received = 3, snapshotReceived = 5, progressed = 2, uploaded = 1, snapshotRemaining = 4, readiness = readiness)
        }

        val fields = traceFields(events.single())
        assertEquals(1L, fields.getValue(SyncTraceField.READY.code.toString()))
        assertEquals(0L, fields.getValue(SyncTraceField.BACKFILL_PENDING.code.toString()))
    }

    @Test fun gatewayBackfillFixtureRecordsPendingWithoutClaimingReady() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            val readiness = BackgroundDiagnostics.gatewayReadiness("""{"contacts_ready":false,"state":"backfill_pending"}""")
            BackgroundDiagnostics.gateway(received = 0, snapshotReceived = 0, progressed = 0, uploaded = 0, snapshotRemaining = 0, readiness = readiness)
        }

        val fields = traceFields(events.single())
        assertEquals(0L, fields.getValue(SyncTraceField.READY.code.toString()))
        assertEquals(1L, fields.getValue(SyncTraceField.BACKFILL_PENDING.code.toString()))
    }

    @Test fun malformedGatewayReadinessIsDroppedWithoutDiagnosticsFailure() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            assertEquals(null, BackgroundDiagnostics.gatewayReadiness("not-json"))
            BackgroundDiagnostics.gateway(received = 0, snapshotReceived = 0, progressed = 0, uploaded = 0, snapshotRemaining = 0, readiness = null)
        }

        val fields = traceFields(events.single())
        assertFalse(fields.containsKey(SyncTraceField.READY.code.toString()))
        assertFalse(fields.containsKey(SyncTraceField.BACKFILL_PENDING.code.toString()))
    }

    @Test fun disabledGatewayReadinessDoesNotParseFixture() {
        SyncDiagnostics.withTestConfiguration(false, { error("clock read") }, { error("CPU read") }, { error("sink called") }) {
            assertEquals(null, BackgroundDiagnostics.gatewayReadiness("not-json"))
        }
    }

    @Test fun contactSocketAndNotificationSummariesUseNumericFields() {
        val events = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 1L }, events::add) {
            BackgroundDiagnostics.contact(
                full = false,
                report = ContactPassReport(ContactPassReport.Outcome.PARTIAL, observed = 7, captured = 5, applied = 2, deleted = 3, more = true),
            )
            BackgroundDiagnostics.socket(
                RemoteMessagingStatus.RECONNECTING,
                reconnects = 4,
                frames = BackgroundDiagnostics.SocketFrameCounts(ready = 1, event = 2, resync = 3, other = 4),
            )
            BackgroundDiagnostics.notification(active = 6, reconciled = 2, dismissed = 1)
        }

        val contact = traceFields(events[0])
        assertEquals(SyncTraceEvent.CONTACT_PROGRESS.code, contact.getValue("event"))
        assertEquals(1L, contact.getValue(SyncTraceField.MORE.code.toString()))
        assertEquals(5L, contact.getValue(SyncTraceField.PROCESSED.code.toString()))
        val socket = traceFields(events[1])
        assertEquals(SyncTraceEvent.SOCKET_STATE.code, socket.getValue("event"))
        assertEquals(4L, socket.getValue(SyncTraceField.COUNT.code.toString()))
        assertEquals(1L, socket.getValue(SyncTraceField.ROWS.code.toString()))
        assertEquals(2L, socket.getValue(SyncTraceField.CAPTURED.code.toString()))
        assertEquals(3L, socket.getValue(SyncTraceField.PROCESSED.code.toString()))
        assertEquals(4L, socket.getValue(SyncTraceField.PENDING.code.toString()))
        val notification = traceFields(events[2])
        assertEquals(SyncTraceEvent.NOTIFICATION_PROGRESS.code, notification.getValue("event"))
        assertEquals(6L, notification.getValue(SyncTraceField.ROWS.code.toString()))
    }

    @Test fun disabledContactSocketAndNotificationSummariesDoNoDiagnosticsWork() {
        SyncDiagnostics.withTestConfiguration(false, { error("clock read") }, { error("CPU read") }, { error("sink called") }) {
            BackgroundDiagnostics.contact(false, ContactPassReport(ContactPassReport.Outcome.PARTIAL, more = true))
            BackgroundDiagnostics.socket(RemoteMessagingStatus.RECONNECTING, reconnects = 4)
            BackgroundDiagnostics.notification(active = 6, reconciled = 2, dismissed = 1)
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
