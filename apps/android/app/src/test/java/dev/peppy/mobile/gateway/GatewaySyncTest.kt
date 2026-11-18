package dev.peppy.mobile.gateway

import dev.peppy.mobile.GatewayAuthException
import dev.peppy.mobile.GatewaySync
import dev.peppy.mobile.LoopbackHttpServer
import dev.peppy.mobile.account.isAccountPolicyDenial
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import dev.peppy.mobile.diagnostics.SyncTraceField
import dev.peppy.mobile.mms.MmsTransferFailure
import dev.peppy.mobile.mms.MmsTransferResult
import dev.peppy.mobile.sms.CallbackIdentity
import dev.peppy.mobile.sms.DispatchSummary
import dev.peppy.mobile.sms.FakeCarrier
import dev.peppy.mobile.sms.SmsDispatcher
import dev.peppy.mobile.telephony.SimRoutes
import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeRawSnapshotRecord
import uniffi.peppy_mobile_bindings.NativeSnapshotManifestEntry
import uniffi.peppy_mobile_bindings.NativeSnapshotManifestProgress
import uniffi.peppy_mobile_bindings.NativeSnapshotProgress
import uniffi.peppy_mobile_bindings.NativeSnapshotProjectionState
import uniffi.peppy_mobile_bindings.NativeSnapshotPurpose
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException


/** A loopback stand-in for the server routes the worker uses; no carrier or real server involved. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewaySyncTest : GatewayTestBase() {
    private val requests = mutableListOf<Pair<String, String>>()
    private val requestHeaders = mutableListOf<Map<String, String>>()
    private var events = JSONArray()
    private var snapshot = JSONArray()
    private var eventsStatus = 200
    private var eventResponder: ((String) -> LoopbackHttpServer.Response)? = null
    private var uploadResponse = LoopbackHttpServer.Response(200, """{"cursor":"9","duplicate":false}""")
    /** Responses consumed (one each) before normal snapshot-record pages are served. */
    private val brokenRecordPages = ArrayDeque<LoopbackHttpServer.Response>()
    /** Responses consumed (one each) before normal manifest pages are served. */
    private val brokenManifestPages = ArrayDeque<LoopbackHttpServer.Response>()
    /** Serves manifest pages with the test's own entries when set; otherwise zero-digest entries (nothing verifies). */
    private var manifestResponder: ((String) -> LoopbackHttpServer.Response?)? = null
    private var unauthorized = false
    /** Overrides the `/v1/snapshot` cut as (high_water_cursor, record_count). */
    private var snapshotCut: Pair<String, String>? = null
    private var compactionCapabilityResponse = LoopbackHttpServer.Response(204)
    private var capabilitiesResponse = LoopbackHttpServer.Response(204)
    private var retentionCapabilityResponse = LoopbackHttpServer.Response(404)
    private var snapshotCompactionSupported = false
    private var snapshotCompactionActive = false
    private var snapshotOriginTagsSupported: Boolean? = null
    private var snapshotManifestSupported: Boolean? = null
    private var maxManifestPageSize = 5_000
    private var uploadResponder: (() -> LoopbackHttpServer.Response)? = null

    private val server = LoopbackHttpServer { request ->
        synchronized(requests) {
            requests += "${request.method} ${request.target}" to request.body
            requestHeaders += request.headers
        }
        val path = request.target.substringBefore('?')
        when {
            unauthorized || request.headers["authorization"] != "Bearer $TEST_TOKEN" -> LoopbackHttpServer.Response(401)
            request.method == "PUT" && path == "/v1/devices/self/retention-capability" -> retentionCapabilityResponse
            request.method == "POST" && path == "/v1/capabilities" -> capabilitiesResponse
            request.method == "POST" && path == "/v1/compaction/capability" -> compactionCapabilityResponse
            request.method == "GET" && path == "/v1/events" ->
                eventResponder?.invoke(request.target) ?: if (eventsStatus == 200) LoopbackHttpServer.Response(200, JSONObject().put("events", events).toString())
                else LoopbackHttpServer.Response(eventsStatus, """{"code":"resync_required"}""")
            request.method == "POST" && path == "/v1/events" -> uploadResponder?.invoke() ?: uploadResponse
            request.method == "GET" && path == "/v1/snapshot" -> LoopbackHttpServer.Response(
                200,
                (snapshotCut ?: (snapshot.length().toString() to snapshot.length().toString())).let { (highWater, count) ->
                    JSONObject()
                        .put("high_water_cursor", highWater)
                        .put("record_count", count)
                        .put("compaction_supported", snapshotCompactionSupported)
                        .put("compaction_active", snapshotCompactionActive)
                        .apply { if (snapshotCompactionSupported) put("compaction_generation", "1") }
                        .apply { snapshotOriginTagsSupported?.let { put("origin_tags_supported", it) } }
                        .apply { snapshotManifestSupported?.let { put("manifest_supported", it).put("max_manifest_page_size", maxManifestPageSize) } }
                        .toString()
                },
            )
            request.method == "GET" && path == "/v1/snapshot/manifest" -> brokenManifestPages.removeFirstOrNull()
                ?: manifestResponder?.invoke(request.target)
                ?: LoopbackHttpServer.Response(200, JSONObject().put("entries", manifestAfter(request.target)).toString())
            request.method == "GET" && path == "/v1/snapshot/records" -> brokenRecordPages.removeFirstOrNull()
                ?: LoopbackHttpServer.Response(200, JSONObject().put("records", recordsAfter(request.target)).toString())
            else -> LoopbackHttpServer.Response(404)
        }
    }

    private fun recordsAfter(target: String): JSONArray {
        val after = Regex("after=([0-9]+)").find(target)!!.groupValues[1].toInt()
        val limit = Regex("limit=([0-9]+)").find(target)!!.groupValues[1].toInt()
        return JSONArray().apply { for (i in after until minOf(after + limit, snapshot.length())) put(snapshot.get(i)) }
    }

    private fun manifestAfter(target: String): JSONArray {
        val after = Regex("after=([0-9]+)").find(target)!!.groupValues[1].toInt()
        val limit = Regex("limit=([0-9]+)").find(target)!!.groupValues[1].toInt()
        return JSONArray().apply {
            for (index in after until minOf(after + limit, snapshot.length())) {
                put(JSONObject().put("cursor", snapshot.getJSONObject(index).getString("cursor")).put("digest", "0".repeat(64)).put("flags", 0))
            }
        }
    }

    @After
    fun stop() = server.close()

    private val state get() = GatewayStateStore(context)
    private val routes = SimRoutes.current(3)
    private val SIX_HOURS_MS = 6 * 60 * 60 * 1000L
    private val SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000L

    private fun capabilityBodies() = requests.filter { it.first == "POST /v1/capabilities" }.map { it.second }

    private fun importingReport() =
        SimRoutes.capabilityReport(emptyList(), sendPermissionGranted = false, setupState = "importing").toString()

    private fun readyReport() = SimRoutes.capabilityReport(routes, sendPermissionGranted = true, setupState = "ready").toString()

    private fun record(cursor: Int, wire: String) = JSONObject().put("cursor", cursor.toString()).put("envelope", JSONObject(wire))

    private fun replayRecords(count: Int): List<JSONObject> {
        val wires = List((count + GatewaySync.EVENT_PAGE - 1) / GatewaySync.EVENT_PAGE) { page ->
            desktopCommandEnvelope(SimRoutes.routeId(3), "receive backlog page $page").second
        }
        return List(count) { index -> record(index + 1, wires[index / GatewaySync.EVENT_PAGE]) }
    }

    private fun serveEventPages(records: List<JSONObject>) {
        eventResponder = { target ->
            val after = Regex("after=([0-9]+)").find(target)!!.groupValues[1].toInt()
            val page = JSONArray()
            records.filter { it.getString("cursor").toInt() > after }.take(50).forEach(page::put)
            LoopbackHttpServer.Response(200, JSONObject().put("events", page).toString())
        }
    }

    private fun sync(
        client: NativeClientInterface,
        carrier: FakeCarrier = FakeCarrier(),
        dispatch: () -> DispatchSummary = { SmsDispatcher(context, client, carrier, routes, sendPermissionGranted = true).dispatch() },
        media: () -> MmsTransferResult = { MmsTransferResult(false, 0, 0, emptyList()) },
        contacts: () -> Boolean = { false },
        contactPhotos: () -> Boolean = { false },
        nowMs: () -> Long = { System.currentTimeMillis() },
        elapsedMs: () -> Long = { 0L },
        sessionCurrent: () -> Boolean = { true },
        effectPermitted: () -> Boolean = { true },
        processPendingSendCancellations: () -> Boolean = { false },
    ) = GatewaySync(
        client, GatewayHttp(server.origin, TEST_TOKEN), state, deviceId,
        dispatch = dispatch,
        capabilities = SimRoutes.capabilityReport(routes, sendPermissionGranted = true),
        media = media,
        contacts = contacts,
        contactPhotos = contactPhotos,
        sessionCurrent = sessionCurrent,
        effectPermitted = effectPermitted,
        processPendingSendCancellations = processPendingSendCancellations,
        nowMs = nowMs,
        elapsedMs = elapsedMs,
    )

    private fun uploads() = requests.count { it.first == "POST /v1/events" }

    private fun envelopeIds(envelopes: List<String>) =
        envelopes.mapTo(linkedSetOf()) { JSONObject(it).getString("envelope_id") }

    private fun assertLocalEvent(envelope: String) {
        val json = JSONObject(envelope)
        assertEquals(deviceId, json.getString("producer_device_id"))
        assertEquals("event", json.getString("purpose"))
    }

    /** A plain envelope (no compaction, no origin_tag) whose `wire_digest` is pinned in the Rust contract test. */
    private val PINNED_PLAIN_ENVELOPE = """{"protocol_version":1,"envelope_id":"00000000-0000-0000-0000-000000000001",""" +
        """"command_id":"00000000-0000-0000-0000-000000000002","vault_id":"00000000-0000-0000-0000-000000000003",""" +
        """"producer_device_id":"00000000-0000-0000-0000-000000000004","producer_sequence":"9","key_epoch":2,""" +
        """"crypto_suite":3,"profile_fingerprint":"${"a".repeat(64)}","purpose":"command",""" +
        """"route":{"gateway_device_id":"00000000-0000-0000-0000-000000000005","subscription_id":"sim:fixture"},""" +
        """"ciphertext":"AQID"}"""
    private val PINNED_PLAIN_DIGEST = "128646e3b468bd6b0a322c287296fae9603530595259c4073fc083fa10a6765d"

    /** Snapshot start requests (capability probe, resync cut) and snapshot pages, in order. */
    private fun snapshotRequests() = requests.map { it.first }.filter { it.startsWith("GET /v1/snapshot") }

    private fun twoSnapshotRecords(): Pair<String, String> {
        val first = desktopCommandEnvelope(SimRoutes.routeId(3), "snapshot one").second
        val second = desktopCommandEnvelope(SimRoutes.routeId(3), "snapshot two").second
        snapshot = JSONArray().put(record(1, first)).put(record(2, second))
        return first to second
    }

    private fun snapshotRecords(count: Int) {
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "large snapshot history")
        snapshot = JSONArray().apply {
            repeat(count) { index -> put(record(index + 1, wire)) }
        }
    }

    /** Leaves a manifest generation staged with cursor 1 downloaded and cursor 2 still missing. */
    private fun interruptManifestResync(client: NativeClientInterface, first: String) {
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenRecordPages += LoopbackHttpServer.Response(200, JSONObject().put("records", JSONArray().put(record(1, first))).toString())
        brokenRecordPages += LoopbackHttpServer.Response(500)
        assertTrue(sync(client).run().failure is GatewayTransportException)
        assertEquals("1", client.snapshotProgress()?.lastCursor)
        eventsStatus = 200
        requests.clear()
    }

    @Test
    fun manifestResyncDownloadsOnlyRecordsReportedMissingByTheManifest() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun serverWithoutManifestSupportKeepsTheFullResyncImport() {
        val client = live()
        twoSnapshotRecords()
        eventsStatus = 409

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=200",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun manifestConflictRestartsFromAFreshCut() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenManifestPages += LoopbackHttpServer.Response(409, """{"code":"resync_required"}""")

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun resumedManifestGenerationContinuesFromItsLastCursor() {
        val client = live()
        val (first, _) = twoSnapshotRecords()
        interruptManifestResync(client, first)

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=1&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=1&limit=1",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun manifestRouteMissingOnResumeFallsBackToFullImport() {
        val client = live()
        val (first, _) = twoSnapshotRecords()
        interruptManifestResync(client, first)
        brokenManifestPages += LoopbackHttpServer.Response(404)

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=1&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=200",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun emptyManifestPageWithNothingMissingFailsTheAttempt() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        repeat(2) { brokenManifestPages += LoopbackHttpServer.Response(200, """{"entries":[]}""") }

        val result = sync(client).run()

        assertTrue(result.failure is GatewayTransportException)
        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
            ),
            snapshotRequests(),
        )
    }

    @Test
    fun recordsConflictInsideAManifestCutRestartsFromAFreshCut() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenRecordPages += LoopbackHttpServer.Response(409, """{"code":"resync_required"}""")

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun manifestRouteMissingOnFreshResyncFallsBackToFullImport() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenManifestPages += LoopbackHttpServer.Response(404)

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=200",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun manifestFallbackStaysFullWhenTheFullCutConflicts() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenManifestPages += LoopbackHttpServer.Response(404)
        brokenRecordPages += LoopbackHttpServer.Response(409, """{"code":"resync_required"}""")

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=200",
                "GET /v1/snapshot",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=200",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun manifestSkipsARecordTheJournalHoldsWhenItsDigestMatches() {
        val client = live()
        // Pinned by `legacy_carrier_envelope()` / `carrier_route_aad_is_byte_stable_fixture` in
        // crates/protocol/tests/protocol_contract.rs: a plain envelope (no compaction, no origin_tag).
        client.ingestRaw(PINNED_PLAIN_ENVELOPE.toByteArray(), "1")
        val (_, second) = desktopCommandEnvelope(SimRoutes.routeId(3), "snapshot two")
        snapshot = JSONArray().put(record(1, PINNED_PLAIN_ENVELOPE)).put(record(2, second))
        snapshotManifestSupported = true
        manifestResponder = {
            LoopbackHttpServer.Response(
                200,
                JSONObject().put(
                    "entries",
                    JSONArray()
                        .put(JSONObject().put("cursor", "1").put("digest", PINNED_PLAIN_DIGEST).put("flags", 0))
                        .put(JSONObject().put("cursor", "2").put("digest", "0".repeat(64)).put("flags", 0)),
                ).toString(),
            )
        }
        eventsStatus = 409

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=1&limit=1",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun malformedManifestEntryRestartsTheCut() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        val numericCursor = JSONObject().put("entries", JSONArray().put(JSONObject().put("cursor", 1).put("digest", "0".repeat(64)).put("flags", 0)))
        brokenManifestPages += LoopbackHttpServer.Response(200, numericCursor.toString())

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun malformedManifestPageRestartsTheCut() {
        val client = live()
        twoSnapshotRecords()
        snapshotManifestSupported = true
        eventsStatus = 409
        brokenManifestPages += LoopbackHttpServer.Response(200, """{"entries":[""")

        assertNull(sync(client).run().failure)

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
            ),
            snapshotRequests(),
        )
        assertNull(client.snapshotProgress())
    }

    @Test
    fun contactRepairResyncUsesTheManifest() {
        val client = live()
        client.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        snapshotManifestSupported = true
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "repair history")
        events = JSONArray().put(record(1, wire))
        snapshot = JSONArray().put(record(1, wire))

        val result = sync(client).run()

        assertNull(result.failure)
        assertFalse(client.contactRepairRequired())
        assertEquals(
            listOf(
                "GET /v1/snapshot/manifest?high_water=1&after=0&limit=5000&compaction_generation=1",
                "GET /v1/snapshot/records?high_water=1&after=0&limit=1&compaction_generation=1",
            ),
            requests.map { it.first }.filter { it.startsWith("GET /v1/snapshot/") },
        )
    }

    @Test
    fun restoreRecoveryNeverRequestsTheManifest() {
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "restore history")
        snapshot = JSONArray().put(record(1, wire))
        snapshotManifestSupported = true
        client.captureIncoming(NativeIncomingSms(null, "+15550007890", "restore", "p-restore", false))
        uploadResponse = LoopbackHttpServer.Response(409, """{"code":"idempotency_conflict"}""")

        sync(client).run()

        assertEquals(SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertTrue(requests.none { it.first.startsWith("GET /v1/snapshot/manifest") })
        assertNotNull(client.snapshotProgress())
        assertFalse(client.snapshotProgress()!!.manifest)
    }

    @Test
    fun pendingCancellationProcessingRunsAfterApplyBeforeDispatch() {
        val client = live()
        val phases = mutableListOf<String>()
        val recordingClient = object : NativeClientInterface by client {
            override fun applyPending(limit: ULong) = client.applyPending(limit).also { phases += "apply" }
        }

        sync(
            recordingClient,
            dispatch = { phases += "dispatch"; DispatchSummary(0, 0, 0, false) },
            processPendingSendCancellations = { phases += "cancellation"; false },
        ).run()

        assertTrue(phases.indexOf("apply") < phases.indexOf("cancellation"))
        assertTrue(phases.indexOf("cancellation") < phases.indexOf("dispatch"))
    }

    @Test
    fun cancellationAfterBlockedOutboxBatchPreventsUploadAndDispatch() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009001", "first", "cancel-batch-1", false))
        client.captureIncoming(NativeIncomingSms(null, "+15550009002", "second", "cancel-batch-2", false))
        val smsEnvelopes = client.pendingOutboxJsonBatch(10uL)
        assertEquals(2, smsEnvelopes.size)
        smsEnvelopes.forEach(::assertLocalEvent)
        val smsEnvelopeIds = envelopeIds(smsEnvelopes)
        val (command, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "must remain pending")
        events = JSONArray().put(record(1, wire))
        val batchStarted = CountDownLatch(1)
        val releaseBatch = CountDownLatch(1)
        val permitted = AtomicBoolean(true)
        val carrier = FakeCarrier()
        var blockedBatch = emptyList<String>()
        val blockingClient = object : NativeClientInterface by client {
            override fun pendingOutboxJsonBatch(maxEnvelopes: ULong): List<String> {
                blockedBatch = client.pendingOutboxJsonBatch(maxEnvelopes)
                batchStarted.countDown()
                assertTrue("test must release batch acquisition", releaseBatch.await(5, TimeUnit.SECONDS))
                return blockedBatch
            }
        }
        var cancellation: Throwable? = null
        val complete = CountDownLatch(1)
        Thread {
            try {
                sync(blockingClient, carrier, effectPermitted = permitted::get).run()
            } catch (error: Throwable) {
                cancellation = error
            } finally {
                complete.countDown()
            }
        }.start()

        assertTrue("upload batch acquisition started", batchStarted.await(5, TimeUnit.SECONDS))
        val blockedIds = envelopeIds(blockedBatch)
        assertEquals("command apply adds one local receipt event", smsEnvelopes.size + 1, blockedBatch.size)
        assertTrue("both SMS envelopes reached the blocked upload", blockedIds.containsAll(smsEnvelopeIds))
        blockedBatch.single { JSONObject(it).getString("envelope_id") !in smsEnvelopeIds }.let(::assertLocalEvent)
        permitted.set(false)
        releaseBatch.countDown()

        assertTrue("cancelled pass completed", complete.await(5, TimeUnit.SECONDS))
        assertTrue(cancellation is CancellationException)
        assertEquals("no POST after cancellation", 0, uploads())
        assertEquals("no envelope was acknowledged", blockedIds, envelopeIds(client.pendingOutboxJsonBatch(10uL)))
        assertTrue("command stayed eligible for dispatch", client.pendingCommands().any { it.commandId == command })
        assertTrue("dispatch phase did not reach the carrier", carrier.calls.isEmpty())
    }

    @Test
    fun cancellationAfterBlockedFirstUploadAcknowledgesIssuedResponseButStopsLaterEffects() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009003", "first", "cancel-post-1", false))
        client.captureIncoming(NativeIncomingSms(null, "+15550009004", "second", "cancel-post-2", false))
        val smsEnvelopes = client.pendingOutboxJsonBatch(10uL)
        assertEquals(2, smsEnvelopes.size)
        smsEnvelopes.forEach(::assertLocalEvent)
        val smsEnvelopeIds = envelopeIds(smsEnvelopes)
        val (command, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "must not dispatch")
        events = JSONArray().put(record(1, wire))
        val firstPostStarted = CountDownLatch(1)
        val releaseFirstPost = CountDownLatch(1)
        val permitted = AtomicBoolean(true)
        val carrier = FakeCarrier()
        var issuedEnvelope = ""
        uploadResponder = {
            issuedEnvelope = synchronized(requests) { requests.last { it.first == "POST /v1/events" }.second }
            firstPostStarted.countDown()
            assertTrue("test must release first POST", releaseFirstPost.await(5, TimeUnit.SECONDS))
            uploadResponse
        }
        var cancellation: Throwable? = null
        val complete = CountDownLatch(1)
        Thread {
            try {
                sync(client, carrier, effectPermitted = permitted::get).run()
            } catch (error: Throwable) {
                cancellation = error
            } finally {
                complete.countDown()
            }
        }.start()

        assertTrue("first POST started", firstPostStarted.await(5, TimeUnit.SECONDS))
        val boundaryBatch = client.pendingOutboxJsonBatch(10uL)
        val boundaryIds = envelopeIds(boundaryBatch)
        val issuedId = JSONObject(issuedEnvelope).getString("envelope_id")
        assertEquals("command apply adds one local receipt event", smsEnvelopes.size + 1, boundaryBatch.size)
        assertTrue("both SMS envelopes reached upload", boundaryIds.containsAll(smsEnvelopeIds))
        boundaryBatch.single { JSONObject(it).getString("envelope_id") !in smsEnvelopeIds }.let(::assertLocalEvent)
        assertTrue("issued envelope came from the captured boundary", issuedId in boundaryIds)
        permitted.set(false)
        releaseFirstPost.countDown()

        assertTrue("cancelled pass completed", complete.await(5, TimeUnit.SECONDS))
        assertTrue(cancellation is CancellationException)
        assertEquals("no later POST", 1, uploads())
        assertEquals(
            "only the issued response was acknowledged",
            boundaryIds - issuedId,
            envelopeIds(client.pendingOutboxJsonBatch(10uL)),
        )
        assertTrue("command stayed eligible for dispatch", client.pendingCommands().any { it.commandId == command })
        assertTrue("dispatch phase did not reach the carrier", carrier.calls.isEmpty())
    }

    @Test
    fun freshEnrollmentBootstrapsFromSnapshotSoHistoricalCommandsNeverReachTheCarrier() {
        val client = enrollAndUnlock()
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        val (historical, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "already handled by someone")
        snapshot = JSONArray().put(record(1, wire))
        events = JSONArray().put(record(1, wire)) // a live replay from 0 would deliver it as live
        client.captureIncoming(NativeIncomingSms(null, "+15550003333", "captured during bootstrap", "p-boot", false))
        val carrier = FakeCarrier()

        val first = sync(client, carrier).run()
        assertNull(first.failure)
        assertTrue("never replays live history at cursor 0", requests.none { it.first.startsWith("GET /v1/events?after=0") })
        assertEquals(SyncPhase.LIVE, state.phase)
        assertTrue(carrier.calls.isEmpty())
        assertEquals(NativePermitState.HISTORICAL, client.beginSendAttempt(historical).state)
        assertEquals("1", client.receiveCursor())
        assertTrue("the bootstrap capture uploads only after the decision", uploads() >= 1)
    }

    // A pass that bootstraps to LIVE selects its branch before bootstrap runs, so the first LIVE
    // branch (retirement, receive, dispatch) is the next pass.
    @Test
    fun swallowedCommandIsRetiredInTheFirstLivePassAndItsStatusUploaded() {
        val client = enrollAndUnlock()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "swallowed during bootstrap")
        snapshot = JSONArray().put(record(1, wire))
        assertNull(sync(client).run().failure)
        assertEquals(SyncPhase.LIVE, state.phase)

        val live = sync(client).run()

        assertNull(live.failure)
        // The first LIVE pass retired it, so nothing is left for a direct call to retire.
        assertEquals(0u, client.retireSwallowedCommands())
        assertTrue("the retirement status is uploaded and acked in the same pass", client.pendingOutboxJsonBatch(10uL).isEmpty())
        assertTrue(uploads() >= 1)
    }

    @Test
    fun liveCommandReceivedInTheRetiringPassIsDispatchedExactlyOnce() {
        val client = enrollAndUnlock()
        val (_, historicalWire) = desktopCommandEnvelope(SimRoutes.routeId(3), "swallowed during bootstrap")
        snapshot = JSONArray().put(record(1, historicalWire))
        val (liveCommand, liveWire) = desktopCommandEnvelope(SimRoutes.routeId(3), "live after bootstrap")
        serveEventPages(listOf(record(2, liveWire)))
        val carrier = FakeCarrier()
        assertNull(sync(client, carrier).run().failure)
        assertTrue("bootstrap alone dispatches nothing", carrier.calls.isEmpty())

        val live = sync(client, carrier).run()

        assertNull(live.failure)
        assertEquals(1, carrier.calls.size)
        val sent = shadowOf(carrier.calls.single().sent.first()).savedIntent
        assertEquals(liveCommand, CallbackIdentity.parse(sent.data)!!.commandId)
        assertEquals(0u, client.retireSwallowedCommands())
    }

    @Test
    fun fullRetirementBatchAsksForAnotherPassAndTheRemainderFinishes() {
        val client = enrollAndUnlock()
        snapshot = JSONArray().apply {
            // One more than the core's retirement batch of 200.
            repeat(201) { index ->
                val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "swallowed $index")
                put(record(index + 1, wire))
            }
        }
        assertNull(sync(client).run().failure)

        val full = sync(client).run()

        assertNull(full.failure)
        assertTrue("a full retirement batch asks for another pass", full.more)
        assertNull(sync(client).run().failure)
        assertEquals(0u, client.retireSwallowedCommands())
    }

    @Test
    fun laterLivePassesRetireNothingMore() {
        val client = enrollAndUnlock()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "swallowed once")
        snapshot = JSONArray().put(record(1, wire))
        assertNull(sync(client).run().failure)
        assertNull(sync(client).run().failure)

        val later = sync(client).run()

        assertNull(later.failure)
        assertEquals(0u, client.retireSwallowedCommands())
        assertTrue(client.pendingOutboxJsonBatch(10uL).isEmpty())
    }

    @Test
    fun bootstrapStagesMoreThanTheFormerPageBudgetInOnePass() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)

        val result = sync(client).run()

        assertNull(result.failure)
        assertFalse(result.more)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertEquals(15, requests.count { it.first.startsWith("GET /v1/snapshot/records") })
    }

    @Test
    fun stagingBudgetContinuesFromDurableProgressWithoutAnotherSnapshotBegin() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)
        var elapsed = 0L
        var stagedPages = 0
        val timedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(
                generation: ULong,
                records: List<NativeRawSnapshotRecord>,
            ) =
                client.appendSnapshotRawPage(generation, records).also {
                    if (++stagedPages == 2) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }

        val first = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(first.failure)
        assertTrue(first.more)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertEquals("400", client.snapshotProgress()?.lastCursor)
        assertEquals(2, requests.count { it.first == "GET /v1/snapshot" })
        assertEquals(
            GatewayStateStore.SetupProgressState(GatewayStateStore.SetupPhase.DOWNLOADING, staged = 400, expected = 3_000),
            state.setupProgress,
        )

        elapsed = 0L
        val second = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(second.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertNull("setup progress is cleared once LIVE", state.setupProgress)
        assertEquals("the second pass only probes compaction capability", 3, requests.count { it.first == "GET /v1/snapshot" })
    }

    @Test
    fun drainBudgetContinuesPublishedSnapshotOnTheNextPass() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)
        var elapsed = 0L
        var applyCalls = 0
        val timedClient = object : NativeClientInterface by client {
            override fun applyPending(limit: ULong) = client.applyPending(limit).also {
                if (++applyCalls == 1) elapsed = GatewaySync.SNAPSHOT_DRAIN_BUDGET_MS + 1
            }
        }

        val first = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(first.failure)
        assertTrue(first.more)
        assertEquals(SyncPhase.BOOTSTRAP_DRAINING, state.phase)
        assertEquals("published snapshot drain stopped at its budget", 1, applyCalls)
        assertNull("the core reports no projection status until the bounded drain completes", client.snapshotProjectionStatus())
        assertEquals(GatewayStateStore.SetupPhase.ORGANIZING, state.setupProgress?.phase)
        assertEquals(listOf(importingReport()), capabilityBodies())

        elapsed = 0L
        val second = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(second.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertNull(state.setupProgress)
        assertEquals(
            "importing while draining, then ready in the pass that goes LIVE",
            listOf(importingReport(), importingReport(), readyReport()),
            capabilityBodies(),
        )
    }

    @Test
    fun stagingCancellationStopsBetweenSnapshotPages() {
        val client = enrollAndUnlock()
        snapshotRecords(600)
        var stagedPages = 0
        val cancellingClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(
                generation: ULong,
                records: List<NativeRawSnapshotRecord>,
            ) =
                client.appendSnapshotRawPage(generation, records).also { stagedPages++ }
        }

        assertThrows(CancellationException::class.java) {
            sync(cancellingClient, effectPermitted = { stagedPages == 0 }).run()
        }
        assertEquals("200", client.snapshotProgress()?.lastCursor)
        assertEquals(1, requests.count { it.first.startsWith("GET /v1/snapshot/records") })
    }

    @Test
    fun liveApplyRemainsBoundedWithoutASnapshot() {
        val client = live()
        var applyCalls = 0
        val recordingClient = object : NativeClientInterface by client {
            override fun applyPending(limit: ULong) = client.applyPending(limit).also { applyCalls++ }
        }

        sync(recordingClient).run()

        assertTrue(applyCalls <= GatewaySync.APPLY_BATCHES)
    }

    @Test
    fun oldHistoryWithOwnProducerRecordsEntersDurableRecoveryWithoutUploadOrCarrier() {
        val client = enrollAndUnlock()
        client.captureIncoming(NativeIncomingSms(null, "+15550004444", "an earlier install's capture", "p-old", false))
        val ownWire = client.pendingOutboxJsonBatch(1uL).single() // producer_device_id == this device
        val (command, commandWire) = desktopCommandEnvelope(SimRoutes.routeId(3), "old command")
        snapshot = JSONArray().put(record(1, commandWire)).put(record(2, ownWire))
        val carrier = FakeCarrier()

        sync(client, carrier).run()
        assertEquals(SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertEquals(RecoveryReason.REUSED_DEVICE_IDENTITY, state.recoveryReason)
        assertEquals(0, uploads())
        assertTrue(carrier.calls.isEmpty())

        requests.clear()
        events = JSONArray().put(record(1, commandWire))
        sync(client, carrier).run()
        assertEquals("durable across passes", SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertEquals(0, uploads())
        assertTrue(carrier.calls.isEmpty())
        assertTrue(requests.none { it.first.startsWith("GET /v1/events") })
        assertEquals(
            "routes are withdrawn",
            SimRoutes.capabilityReport(emptyList(), false).toString(),
            requests.single { it.first == "POST /v1/capabilities" }.second,
        )
        assertTrue(client.beginSendAttempt(command).state != NativePermitState.PERMIT)
        assertEquals(SyncPhase.RECOVERY_REQUIRED, NativeGateway.status(context).syncPhase)
    }

    @Test
    fun manifestBootstrapStillDetectsReusedDeviceIdentity() {
        val client = enrollAndUnlock()
        client.captureIncoming(NativeIncomingSms(null, "+15550004445", "an earlier install's capture", "p-old-manifest", false))
        val ownWire = client.pendingOutboxJsonBatch(1uL).single()
        val (_, commandWire) = desktopCommandEnvelope(SimRoutes.routeId(3), "old command")
        snapshot = JSONArray().put(record(1, commandWire)).put(record(2, ownWire))
        snapshotManifestSupported = true

        sync(client).run()

        assertEquals(
            listOf(
                "GET /v1/snapshot",
                "GET /v1/snapshot",
                "GET /v1/snapshot/manifest?high_water=2&after=0&limit=5000",
                "GET /v1/snapshot/records?high_water=2&after=0&limit=2",
                "GET /v1/snapshot",
            ),
            snapshotRequests(),
        )
        assertEquals(SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertEquals(RecoveryReason.REUSED_DEVICE_IDENTITY, state.recoveryReason)
    }

    private fun live(): NativeClient = enrollAndUnlock().also { state.advance(SyncPhase.LIVE) }

    @Test
    fun declaresCompactionCapabilityBeforeProbingSnapshot() {
        sync(live()).run()
        assertTrue(
            requests.indexOfFirst { it.first == "POST /v1/compaction/capability" } <
                requests.indexOfFirst { it.first == "GET /v1/snapshot" },
        )
    }

    @Test
    fun missingCompactionCapabilityKeepsLegacySyncWorking() {
        compactionCapabilityResponse = LoopbackHttpServer.Response(404)
        val client = live()

        val result = sync(client).run()

        assertNull(result.failure)
        assertFalse(client.serverCompactionSupported())
        assertTrue(requests.any { it.first == "GET /v1/events?after=0&limit=50" })
    }

    @Test fun snapshotOriginTagsCapabilityDefaultsToFalseWhenMissing() {
        val client = live()
        val values = mutableListOf<Boolean>()
        val recordingClient = object : NativeClientInterface by client {
            override fun setServerOriginTagsSupported(supported: Boolean) {
                values += supported
                client.setServerOriginTagsSupported(supported)
            }
        }

        sync(recordingClient).run()

        assertEquals(listOf(false, false), values)
    }

    @Test fun snapshotOriginTagsCapabilityRecordsTrue() {
        val client = live()
        snapshotOriginTagsSupported = true
        val values = mutableListOf<Boolean>()
        val recordingClient = object : NativeClientInterface by client {
            override fun setServerOriginTagsSupported(supported: Boolean) {
                values += supported
                client.setServerOriginTagsSupported(supported)
            }
        }

        sync(recordingClient).run()

        assertEquals(listOf(false, true), values)
    }

    @Test fun snapshotOriginTagsCapabilityRecordsFalse() {
        val client = live()
        client.setServerOriginTagsSupported(true)
        snapshotOriginTagsSupported = false
        val values = mutableListOf<Boolean>()
        val recordingClient = object : NativeClientInterface by client {
            override fun setServerOriginTagsSupported(supported: Boolean) {
                values += supported
                client.setServerOriginTagsSupported(supported)
            }
        }

        sync(recordingClient).run()

        assertEquals(listOf(false, false), values)
    }

    @Test
    fun contactRepairOnLegacyServerDoesNotBlockSmsRelay() {
        val client = live()
        // The same durable latch is set when an existing database upgrades.
        client.requestContactRepair()
        compactionCapabilityResponse = LoopbackHttpServer.Response(404)
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "SMS must still send")
        events = JSONArray().put(record(1, wire))
        client.captureIncoming(NativeIncomingSms(null, "+15550005555", "inbound during contact repair", "repair-inbound", false))
        val carrier = FakeCarrier()
        var contactCalls = 0

        val result = sync(client, carrier, contacts = { contactCalls++; false }).run()

        assertNull(result.failure)
        assertEquals(1, carrier.calls.size)
        assertTrue(uploads() > 0)
        assertTrue(requests.any { it.first.startsWith("GET /v1/events") })
        assertTrue(client.contactRepairRequired())
        assertEquals(0, contactCalls)
    }

    @Test
    fun unreadSmsIsReceivedLiveBeforeContactRepairSnapshot() {
        val client = live()
        client.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "new command before contact repair")
        events = JSONArray().put(record(1, wire))
        snapshot = JSONArray().put(record(1, wire))
        val carrier = FakeCarrier()

        val result = sync(client, carrier).run()

        assertNull(result.failure)
        assertEquals(1, carrier.calls.size)
        assertFalse(client.contactRepairRequired())
    }

    @Test
    fun manualContactRepairUsesEmptyAuthoritativeSnapshotAndClearsTheLatch() {
        val client = live()
        client.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true

        val result = sync(client).run()

        assertNull(result.failure)
        assertFalse(client.contactRepairRequired())
        assertEquals(NativeSnapshotProjectionState.PROMOTED, client.snapshotProjectionStatus()?.state)
    }

    @Test
    fun partialContactRepairSnapshotBlocksContactCallback() {
        val client = live()
        client.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "staged repair history")
        snapshot = JSONArray().apply { repeat(801) { put(record(it + 1, wire)) } }
        var contactCalls = 0

        val result = sync(client, contacts = { contactCalls++; false }).run()

        assertNull(result.failure)
        assertTrue(result.more)
        assertTrue(client.contactRepairRequired())
        assertEquals(0, contactCalls)
    }

    @Test
    fun failedContactRepairProjectionDoesNotRunContactCallback() {
        val client = live()
        client.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "corrupt repair history")
        val desktop = SharedVault.desktop
        desktop.setServerCompactionState(true, true)
        val producer = JSONObject(wire).getString("producer_device_id")
        desktop.captureContactBook(JSONObject().put("schema_version", 1)
            .put("book", JSONObject().put("id", "repair-book").put("owner_device_id", producer).put("generation", "1").put("state", "active"))
            .put("contacts", JSONArray()).toString())
        val contactEvent = desktop.pendingOutboxJson().map(::JSONObject).last { it.has("compaction") }
        desktop.ackOutbox(contactEvent.getString("envelope_id"))
        // A malformed historical command can be quarantined without invalidating contacts.
        // A corrupt compactable contact record must fail the authoritative projection.
        val (_, sms) = desktopCommandEnvelope(SimRoutes.routeId(3), "SMS after failed contact repair")
        client.ingestRaw(sms.toByteArray(), "1")
        client.applyPending(50uL)
        val corrupt = contactEvent.put("ciphertext", "AAAA").toString()
        snapshot = JSONArray().put(record(1, sms)).put(record(2, corrupt))
        events = JSONArray().put(record(2, corrupt))
        client.captureIncoming(NativeIncomingSms(null, "+15550005555", "upload despite repair failure", "failed-repair-inbound", false))
        val carrier = FakeCarrier()
        var contactCalls = 0

        val result = sync(client, carrier, contacts = { contactCalls++; false }).run()

        assertTrue(result.failure is GatewayTransportException)
        assertTrue(client.contactRepairRequired())
        assertEquals(NativeSnapshotProjectionState.FAILED, client.snapshotProjectionStatus()?.state)
        assertEquals(0, contactCalls)
        assertEquals(1, carrier.calls.size)
        assertTrue(uploads() > 0)
    }

    @Test
    fun receiveFailureDoesNotBlockUpload() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550005555", "must still upload", "p-5xx", false))
        eventsStatus = 503
        val result = sync(client).run()
        assertNotNull("transient failure is reported for retry", result.failure)
        assertEquals(1, uploads())
        assertTrue(client.pendingOutboxJsonBatch(10uL).isEmpty())
    }

    @Test
    fun receiveDrains120EventsAcrossThreePagesInOnePass() {
        val client = live()
        serveEventPages(replayRecords(120))

        val result = sync(client).run()

        assertFalse(result.more)
        assertTrue(result.commandsProgressed)
        assertEquals("120", client.receiveCursor())
        assertEquals(3, requests.count { it.first.startsWith("GET /v1/events") })
    }

    @Test
    fun receiveLeavesMoreAfterFourFullPages() {
        val client = live()
        serveEventPages(replayRecords(200))

        val result = sync(client).run()

        assertTrue(result.more)
        assertTrue(result.commandsProgressed)
        assertEquals("200", client.receiveCursor())
        assertEquals(4, requests.count { it.first.startsWith("GET /v1/events") })
    }

    @Test
    fun receiveStopsAfterAShortFirstPage() {
        val client = live()
        serveEventPages(replayRecords(20))

        val result = sync(client).run()

        assertFalse(result.more)
        assertEquals("20", client.receiveCursor())
        assertEquals(1, requests.count { it.first.startsWith("GET /v1/events") })
    }

    @Test
    fun receiveStartsResyncWhenTheSecondPageReturns409() {
        val client = live()
        val firstPage = replayRecords(50)
        var eventRequests = 0
        eventResponder = {
            if (++eventRequests == 1) LoopbackHttpServer.Response(200, JSONObject().put("events", JSONArray(firstPage)).toString())
            else LoopbackHttpServer.Response(409, """{"code":"resync_required"}""")
        }

        val result = sync(client).run()

        assertNull(result.failure)
        assertEquals(2, eventRequests)
        assertEquals("50", client.receiveCursor())
        assertTrue(requests.any { it.first == "GET /v1/snapshot" })
    }

    @Test
    fun receiveStopsWhenAFullPageDoesNotAdvanceTheCursor() {
        val client = live()
        val page = JSONArray(replayRecords(50))
        eventResponder = { LoopbackHttpServer.Response(200, JSONObject().put("events", page).toString()) }

        val result = sync(client).run()

        assertTrue(result.more)
        assertEquals("50", client.receiveCursor())
        assertEquals(2, requests.count { it.first.startsWith("GET /v1/events") })
    }

    @Test
    fun applyProgressAfterAJournalFailureRequestsTheNativeRcsWake() {
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "apply after journal failure")
        events = JSONArray().put(record(1, wire))
        var applyCalls = 0
        val transientApplyFailure = object : NativeClientInterface by client {
            override fun applyPending(limit: ULong) = if (++applyCalls == 2) {
                throw MobileBindingsException.InvalidRequest()
            } else {
                client.applyPending(limit)
            }
        }

        assertTrue(sync(transientApplyFailure).run().commandsProgressed)
        events = JSONArray()

        assertTrue(sync(transientApplyFailure).run().commandsProgressed)
    }

    @Test fun `carrier proof blocked dispatch is exposed without command apply progress`() {
        val result = sync(live(), dispatch = { DispatchSummary(0, 0, 1, false, carrierOnlyProofBlocked = 1) }).run()

        assertFalse(result.commandsProgressed)
        assertEquals(1, result.carrierOnlyProofBlocked)
    }

    @Test
    fun transientMediaFailureStillUploadsAndDispatchesSmsWithoutRetryingTheGatewayPass() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009991", "unrelated envelope", "p-media", false))
        val (commandId, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "unrelated SMS")
        events = JSONArray().put(record(1, wire))
        val carrier = FakeCarrier()
        val phases = mutableListOf<String>()
        val sync = GatewaySync(
            client, GatewayHttp(server.origin, TEST_TOKEN), state, deviceId,
            dispatch = {
                phases += "dispatch"
                SmsDispatcher(context, client, carrier, routes, sendPermissionGranted = true).dispatch()
            },
            capabilities = SimRoutes.capabilityReport(routes, sendPermissionGranted = true),
            media = {
                phases += "media"
                MmsTransferResult(false, 0, 0, listOf(MmsTransferFailure("00000000-0000-0000-0000-000000000001", "upload", "transient")))
            },
        )

        val result = sync.run()

        assertNull("media retry is isolated from the productive gateway pass", result.failure)
        assertFalse("a failed media batch does not enqueue an immediate successor", result.more)
        assertEquals(listOf("media", "dispatch"), phases)
        assertTrue("receive completed before media", requests.any { it.first == "GET /v1/events?after=0&limit=50" })
        assertTrue("unrelated outbox upload is not starved", uploads() >= 1)
        assertEquals(commandId, CallbackIdentity.parse(shadowOf(carrier.calls.single().sent.first()).savedIntent.data)!!.commandId)
    }

    @Test
    fun repeatedTransientMediaFailuresDoNotBlockOutboxBacklogProgress() {
        val client = live()
        repeat(GatewaySync.OUTBOX_BATCH * 2 + 1) { index ->
            client.captureIncoming(NativeIncomingSms(null, "+15550009993", "backlog $index", "p-media-$index", false))
        }
        var mediaPasses = 0
        val media = {
            mediaPasses++
            MmsTransferResult(false, 0, 0, listOf(MmsTransferFailure("00000000-0000-0000-0000-000000000003", "upload", "transient")))
        }

        val first = sync(client, media = media).run()

        assertNull(first.failure)
        assertTrue("upload backlog still requests its bounded successor", first.more)
        assertEquals(GatewaySync.OUTBOX_BATCH * 2, uploads())

        val second = sync(client, media = media).run()

        assertNull(second.failure)
        assertEquals(2, mediaPasses)
        assertEquals(GatewaySync.OUTBOX_BATCH * 2 + 1, uploads())
        assertTrue(client.pendingOutboxJsonBatch(1_000uL).isEmpty())
    }

    @Test
    fun mixedMediaBatchKeepsItsRemainingBacklogWithoutRetryingTheGatewayPass() {
        val client = live()

        val result = sync(
            client,
            media = {
                MmsTransferResult(
                    more = true,
                    uploaded = 0,
                    downloaded = 1,
                    failures = listOf(MmsTransferFailure("00000000-0000-0000-0000-000000000004", "upload", "transient")),
                )
            },
        ).run()

        assertNull("the media failure must not retry the productive gateway pass", result.failure)
        assertTrue("remaining media work retains its bounded successor", result.more)
    }

    @Test
    fun failedMediaDownloadsDoNotContinueWithoutTransferProgress() {
        val client = live()

        val result = sync(
            client,
            media = {
                MmsTransferResult(
                    more = true,
                    uploaded = 0,
                    downloaded = 0,
                    failures = listOf(MmsTransferFailure("00000000-0000-0000-0000-000000000005", "download", "transient")),
                )
            },
        ).run()

        assertNull("failed media must not retry the productive gateway pass", result.failure)
        assertFalse("no transfer progress means no immediate successor", result.more)
    }

    @Test
    fun mediaAuthFailureAbortsBeforeUploadOrCarrierDispatch() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009992", "must remain queued", "p-media-auth", false))
        var dispatched = false
        var thrown: Throwable? = null
        try {
            GatewaySync(
                client, GatewayHttp(server.origin, TEST_TOKEN), state, deviceId,
                dispatch = { dispatched = true; DispatchSummary(0, 0, 0, false) },
                capabilities = SimRoutes.capabilityReport(routes, sendPermissionGranted = true),
                media = { MmsTransferResult(false, 0, 0, listOf(MmsTransferFailure("00000000-0000-0000-0000-000000000002", "upload", "auth"))) },
            ).run()
        } catch (error: GatewayAuthException) {
            thrown = error
        }
        assertTrue(thrown is GatewayAuthException)
        assertFalse(dispatched)
        assertEquals(0, uploads())
    }

    @Test
    fun bootstrapNeverRunsMediaPhase() {
        val client = enrollAndUnlock()
        var mediaCalls = 0
        sync(client, media = { mediaCalls++; MmsTransferResult(false, 0, 0, emptyList()) }).run()
        assertEquals(0, mediaCalls)
    }

    @Test
    fun impossibleSnapshotCutRestagesFromAFreshCut() {
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "resync history")
        snapshot = JSONArray().put(record(1, wire))
        eventsStatus = 409
        brokenRecordPages += LoopbackHttpServer.Response(409, """{"code":"resync_required"}""")
        val result = sync(client).run()
        assertNull(result.failure)
        // One capability probe plus the original and replacement snapshot cuts.
        assertEquals(3, requests.count { it.first == "GET /v1/snapshot" })
        assertNull("restaged generation finished", client.snapshotProgress())
        eventsStatus = 200
        sync(client).run()
        assertEquals("1", client.receiveCursor())
    }

    @Test
    fun repeatedImpossibleCutIsReportedForBoundedRetry() {
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "x")
        snapshot = JSONArray().put(record(1, wire))
        eventsStatus = 409
        repeat(2) { brokenRecordPages += LoopbackHttpServer.Response(200, """{"records":[]}""") }
        assertTrue(sync(client).run().failure is GatewayTransportException)
    }

    @Test
    fun uploadIdempotencyConflictEntersRecoveryAndStopsCarrierWork() {
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "would be sent")
        events = JSONArray().put(record(1, wire))
        client.captureIncoming(NativeIncomingSms(null, "+15550006666", "conflicting", "p-409", false))
        uploadResponse = LoopbackHttpServer.Response(409, """{"code":"idempotency_conflict"}""")
        val carrier = FakeCarrier()
        sync(client, carrier).run()
        assertEquals(SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertEquals(RecoveryReason.PRODUCER_CONFLICT, state.recoveryReason)
        assertTrue(carrier.calls.isEmpty())
        assertTrue("never acked", client.pendingOutboxJsonBatch(10uL).isNotEmpty())
    }

    @Test
    fun permanentUploadRejectionIsDistinctTerminalAndNeverAcked() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007777", "rejected", "p-422", false))
        uploadResponse = LoopbackHttpServer.Response(422, """{"code":"invalid_envelope"}""")
        val carrier = FakeCarrier()
        val result = sync(client, carrier).run()
        assertNull(result.failure)
        assertFalse(result.more)
        assertEquals("invalid_envelope", state.outboxRejection)
        assertEquals(1, client.pendingOutboxJsonBatch(10uL).size)
        requests.clear()
        sync(client, carrier).run()
        assertEquals("no retry loop", 0, uploads())
        assertEquals("invalid_envelope", NativeGateway.status(context).outboxRejection)
    }

    @Test
    fun codeLessUploadRejectionIsSoftNeverAckedAndRetriedOnNextPass() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007878", "soft", "p-soft-403", false))
        uploadResponse = LoopbackHttpServer.Response(403)
        val result = sync(client).run()
        assertNull(result.failure)
        assertNull(state.outboxRejection)
        assertEquals("http_403", state.softRejection?.code)
        assertEquals("http_403", NativeGateway.status(context).softRejection?.code)
        assertEquals(1, client.pendingOutboxJsonBatch(10uL).size)
        assertEquals("one POST per pass while soft-rejected", 1, uploads())

        requests.clear()
        uploadResponse = LoopbackHttpServer.Response(200, """{"cursor":"9","duplicate":false}""")
        sync(client).run()
        assertEquals("the next pass retries the head envelope once", 1, uploads())
        assertNull(state.softRejection)
        assertTrue(client.pendingOutboxJsonBatch(10uL).isEmpty())
    }

    @Test
    fun emptyOutboxClearsSoftUploadRejectionWithoutAcking() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007882", "soft", "p-soft-empty", false))
        uploadResponse = LoopbackHttpServer.Response(403)

        sync(client).run()
        assertNotNull(state.softRejection)
        assertEquals(1, client.pendingOutboxJsonBatch(10uL).size)

        val emptyOutboxClient = object : NativeClientInterface by client {
            override fun pendingOutboxJsonBatch(maxEvents: ULong) = emptyList<String>()
        }
        sync(emptyOutboxClient).run()

        assertNull(state.softRejection)
        assertEquals("nothing was acked", 1, client.pendingOutboxJsonBatch(10uL).size)
    }

    @Test
    fun softUploadRejectionDoesNotHideDispatchBacklog() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007883", "soft", "p-soft-dispatch", false))
        uploadResponse = LoopbackHttpServer.Response(403)
        sync(client).run()

        val result = sync(client, dispatch = { DispatchSummary(0, 0, 0, true) }).run()

        assertTrue(result.more)
    }

    @Test
    fun softUploadRejectionSkipsOnlyContactPhotosAndRecoveryRequeuesThem() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007884", "soft", "p-soft-photos", false))
        var mediaCalls = 0
        var contactCalls = 0
        var contactPhotoCalls = 0
        val media = { mediaCalls++; MmsTransferResult(false, 0, 0, emptyList()) }
        val contacts = { contactCalls++; false }
        val contactPhotos = { contactPhotoCalls++; false }
        uploadResponse = LoopbackHttpServer.Response(403)
        sync(client, media = media, contacts = contacts, contactPhotos = contactPhotos).run()

        mediaCalls = 0
        contactCalls = 0
        contactPhotoCalls = 0
        sync(client, media = media, contacts = contacts, contactPhotos = contactPhotos).run()

        assertEquals(1, mediaCalls)
        assertEquals(1, contactCalls)
        assertEquals(0, contactPhotoCalls)

        uploadResponse = LoopbackHttpServer.Response(200, """{"cursor":"9","duplicate":false}""")
        val recovering = sync(client, media = media, contacts = contacts, contactPhotos = contactPhotos).run()

        assertTrue(recovering.more)
        assertEquals(1, contactPhotoCalls)
    }

    private val policyDenial = LoopbackHttpServer.Response(403, """{"code":"operation_not_permitted"}""")

    @Test
    fun codedPolicyDenialOnUploadPausesWithoutRejectingOrAckingEnvelopes() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550008801", "paused upload", "p-policy-upload", false))
        uploadResponse = policyDenial
        val result = sync(client).run()
        assertNull(result.failure)
        assertTrue(result.accountPaused)
        assertNull(state.outboxRejection)
        assertNull(state.softRejection)
        assertEquals("one POST per pass while paused", 1, uploads())
        assertEquals("envelope stays pending, unacked", 1, client.pendingOutboxJsonBatch(10uL).size)
    }

    @Test
    fun capabilityPolicyDenialSkipsPublishAndKeepsReceiving() {
        val client = live()
        capabilitiesResponse = policyDenial
        val result = sync(client).run()
        assertNull(result.failure)
        assertTrue(result.accountPaused)
        assertTrue(requests.any { it.first.startsWith("GET /v1/events?after=") })
    }

    @Test
    fun compactionPolicyDenialFallsBackAndKeepsReceiving() {
        val client = live()
        compactionCapabilityResponse = policyDenial
        val result = sync(client).run()
        assertNull(result.failure)
        assertTrue(result.accountPaused)
        assertTrue(requests.any { it.first.startsWith("GET /v1/events?after=") })
    }

    @Test
    fun retentionPolicyDenialDefersRetentionWithoutFailingThePass() {
        val client = live()
        retentionCapabilityResponse = policyDenial
        val result = sync(client).run()
        assertNull(result.failure)
        assertTrue(result.accountPaused)
        assertFalse("producers wait for retention", result.producersReady)
        assertTrue(requests.any { it.first.startsWith("GET /v1/events?after=") })
    }

    @Test
    fun unauthorizedUploadStillThrowsAuthFailure() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550008802", "unauthorized", "p-401", false))
        uploadResponse = LoopbackHttpServer.Response(401)
        assertThrows(GatewayAuthException::class.java) { sync(client).run() }
        assertNull(state.outboxRejection)
    }

    @Test
    fun onlyTheCodedOperationNotPermittedIsAPolicyDenial() {
        assertTrue(HttpResult(403, """{"code":"operation_not_permitted"}""").isAccountPolicyDenial())
        assertFalse(HttpResult(403, null).isAccountPolicyDenial())
        assertFalse(HttpResult(403, """{"code":"producer_not_authorized"}""").isAccountPolicyDenial())
        assertFalse(HttpResult(401, """{"code":"operation_not_permitted"}""").isAccountPolicyDenial())
    }

    @Test
    fun codedUploadRejectionStaysPermanentWithoutSoftState() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007879", "coded", "p-coded-403", false))
        uploadResponse = LoopbackHttpServer.Response(403, """{"code":"producer_not_authorized"}""")
        sync(client).run()
        assertEquals("producer_not_authorized", state.outboxRejection)
        assertNull(state.softRejection)
    }

    @Test
    fun shortFuseSoftRejectionEscalatesAfterOneHourAndThreeAttempts() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007880", "unprocessable", "p-soft-422", false))
        uploadResponse = LoopbackHttpServer.Response(422)
        var now = 1_000_000L
        fun attempt() = sync(client, nowMs = { now }).run()
        attempt()
        now += 30 * 60_000L
        attempt()
        assertNull("two attempts, 30 min: not yet", state.outboxRejection)
        now += 30 * 60_000L
        attempt()
        assertEquals("http_422", state.outboxRejection)
        assertNull(state.softRejection)
        assertEquals("never acked", 1, client.pendingOutboxJsonBatch(10uL).size)
    }

    @Test
    fun longFuseSoftRejectionEscalatesOnlyAfterFortyEightHoursAndTenAttempts() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550007881", "forbidden", "p-soft-403-long", false))
        uploadResponse = LoopbackHttpServer.Response(403)
        var now = 1_000_000L
        fun attempt() = sync(client, nowMs = { now }).run()
        repeat(9) { attempt() }
        now += 47 * 3_600_000L
        attempt()
        assertEquals("ten attempts but only 47 h old", 10, state.softRejection?.attempts)
        assertNull(state.outboxRejection)
        now += 3_600_000L
        attempt()
        assertEquals("http_403", state.outboxRejection)
        assertNull(state.softRejection)
    }

    @Test fun diagnosticsAckIsRecordedOnlyAfterSuccessfulDurableAck() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009991", "ack success", "diag-ack", false))
        val traces = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) {
            assertNull(sync(client).run().failure)
        }
        assertTrue(client.pendingOutboxJsonBatch(10uL).isEmpty())
        assertEquals(1, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
    }

    @Test fun diagnosticsAckIsAbsentForTransientUpload() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009992", "ack rejected", "diag-ack-500", false))
        uploadResponse = LoopbackHttpServer.Response(500, "{\"code\":\"rejected\"}")
        val traces = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(client).run() }
        assertEquals(0, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
        assertTrue(client.pendingOutboxJsonBatch(10uL).isNotEmpty())
    }

    @Test fun diagnosticsAckIsAbsentWhenDurableAckThrowsAndOriginalFailureEscapes() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009993", "ack throw", "diag-ack-throw", false))
        val failure = IllegalStateException("durable ack failed")
        val throwingClient = object : NativeClientInterface by client {
            override fun ackOutbox(envelopeId: String) = throw failure
        }
        val traces = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) {
            try {
                sync(throwingClient).run()
                throw AssertionError("expected durable ack failure")
            } catch (caught: IllegalStateException) {
                assertTrue(caught === failure)
            }
        }

        assertEquals(0, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
        assertTrue(client.pendingOutboxJsonBatch(10uL).isNotEmpty())
    }

    @Test fun diagnosticsAckIsAbsentForIdempotencyConflict() {
        val traces = mutableListOf<String>()
        val conflictClient = live()
        conflictClient.captureIncoming(NativeIncomingSms(null, "+15550009994", "conflict", "diag-ack-409", false))
        uploadResponse = LoopbackHttpServer.Response(409, "{\"code\":\"idempotency_conflict\"}")
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(conflictClient).run() }
        assertEquals(SyncPhase.RECOVERY_REQUIRED, state.phase)
        assertTrue(conflictClient.pendingOutboxJsonBatch(10uL).isNotEmpty())
        assertEquals(0, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
    }

    @Test fun diagnosticsAckIsAbsentForPermanentRejection() {
        val traces = mutableListOf<String>()
        val rejectionClient = live()
        rejectionClient.captureIncoming(NativeIncomingSms(null, "+15550009995", "rejection", "diag-ack-422", false))
        uploadResponse = LoopbackHttpServer.Response(422, "{\"code\":\"invalid_envelope\"}")
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(rejectionClient).run() }
        assertEquals("invalid_envelope", state.outboxRejection)
        assertTrue(rejectionClient.pendingOutboxJsonBatch(10uL).isNotEmpty())
        assertEquals(0, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
    }

    @Test fun diagnosticsAckIsAbsentForBadRequestRejection() {
        val client = live()
        client.captureIncoming(NativeIncomingSms(null, "+15550009996", "bad request", "diag-ack-400", false))
        uploadResponse = LoopbackHttpServer.Response(400, "{\"code\":\"invalid_envelope\"}")
        val traces = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(client).run() }
        assertEquals("invalid_envelope", state.outboxRejection)
        assertTrue(client.pendingOutboxJsonBatch(10uL).isNotEmpty())
        assertEquals(0, traces.count { traceEvent(it) == SyncTraceEvent.OUTBOX_ACK.code })
    }

    @Test fun malformedDiagnosticReadinessDoesNotRepeatProbeOrChangePassOutcome() {
        val client = live()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        var compactionStateCalls = 0
        val malformedReadinessClient = object : NativeClientInterface by client {
            override fun setServerCompactionState(supported: Boolean, active: Boolean): String {
                compactionStateCalls++
                client.setServerCompactionState(supported, active)
                return "not-json"
            }
        }
        val traces = mutableListOf<String>()

        val result = SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) {
            sync(malformedReadinessClient).run()
        }

        assertNull(result.failure)
        assertFalse(result.more)
        assertEquals(1, compactionStateCalls)
        assertEquals(1, requests.count { it.first == "POST /v1/compaction/capability" })
        assertEquals("a LIVE pass fetches the cut once, for the compaction probe", 1, requests.count { it.first == "GET /v1/snapshot" })
        assertTrue(requests.none { it.first.startsWith("GET /v1/snapshot/records") })
        val progress = traces.map(::traceFields).single { it["event"] == SyncTraceEvent.GATEWAY_PROGRESS.code }
        assertFalse(progress.containsKey(SyncTraceField.READY.code.toString()))
        assertFalse(progress.containsKey(SyncTraceField.BACKFILL_PENDING.code.toString()))
    }

    @Test fun capacityBookkeepingFailureDoesNotRevokeVerifiedCompactionReadiness() {
        val client = live()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        val states = mutableListOf<Pair<Boolean, Boolean>>()
        val bookkeepingFailure = object : NativeClientInterface by client {
            override fun setServerCompactionState(supported: Boolean, active: Boolean): String {
                states += supported to active
                return client.setServerCompactionState(supported, active)
            }
            override fun recordRecoverySnapshotCapacity(highWaterCursor: String, recordCount: ULong) {
                error("test local bookkeeping failure")
            }
        }
        val result = sync(bookkeepingFailure).run()
        assertNull(result.failure)
        assertEquals(listOf(true to true), states)
        assertEquals(1, requests.count { it.first == "GET /v1/snapshot" })
    }

    private fun traceEvent(line: String): Long? = line.split(' ').firstOrNull { it.startsWith("event=") }
        ?.substringAfter('=')?.toLongOrNull()

    private fun traceFields(line: String): Map<String, Long> = line.split(' ').associate { token ->
        val (key, value) = token.split('=', limit = 2)
        key to value.toLong()
    }

    @Test
    fun negotiatedProtocolHeadersSteadyStateMutationsAcrossTwoPasses() {
        retentionCapabilityResponse = LoopbackHttpServer.Response(204)
        val client = live()

        assertNull(sync(client).run().failure)
        assertNull(sync(client).run().failure)

        val capabilityIndexes = requests.indices.filter { requests[it].first == "POST /v1/capabilities" }
        assertEquals(2, capabilityIndexes.size)
        capabilityIndexes.forEach { index ->
            assertEquals("1", requestHeaders[index]["peppy-attachment-retention-protocol"])
        }
        val negotiationIndex = requests.indexOfFirst { it.first == "PUT /v1/devices/self/retention-capability" }
        assertTrue(negotiationIndex < capabilityIndexes.first())
        assertNull(requestHeaders[negotiationIndex]["peppy-attachment-retention-protocol"])
    }

    @Test
    fun transientRetentionNegotiationDefersProducersButKeepsSafeLiveReceiveAndCarrier() {
        retentionCapabilityResponse = LoopbackHttpServer.Response(503, """{"code":"busy"}""")
        val client = live()
        val (_, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "safe carrier command")
        events = JSONArray().put(record(1, wire))
        val carrier = FakeCarrier()
        var mediaCalls = 0

        val result = sync(client, carrier, media = { mediaCalls++; MmsTransferResult(false, 0, 0, emptyList()) }).run()

        assertTrue(result.failure is GatewayTransportException)
        assertEquals(1, mediaCalls)
        assertEquals(1, carrier.calls.size)
        assertTrue(requests.any { it.first == "GET /v1/events?after=0&limit=50" })
        assertTrue(requests.any { it.first == "POST /v1/events" })
    }

    @Test
    fun staleRetentionResponseCancelsTheWholeObsoletePass() {
        val client = live()

        val result = sync(client, sessionCurrent = { false }).run()

        assertNull(result.failure)
        assertFalse(result.more)
        assertEquals(listOf("PUT /v1/devices/self/retention-capability"), requests.map { it.first })
    }

    @Test
    fun livePassReplaysUploadsAndDispatchesOnce() {
        val client = live()
        val (commandId, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "from desktop")
        events = JSONArray().put(record(1, wire))
        val carrier = FakeCarrier()
        val result = sync(client, carrier).run()
        assertNull(result.failure)
        assertEquals(
            SimRoutes.capabilityReport(routes, true, setupState = "ready").toString(),
            requests.first { it.first == "POST /v1/capabilities" }.second,
        )
        assertTrue(requests.any { it.first == "GET /v1/events?after=0&limit=50" })
        val sent = shadowOf(carrier.calls.single().sent.first()).savedIntent
        assertEquals(commandId, CallbackIdentity.parse(sent.data)!!.commandId)
        assertTrue("SUBMITTED status uploaded and acked", client.pendingOutboxJsonBatch(10uL).isEmpty())
        assertEquals("1", client.receiveCursor())
    }

    @Test
    fun rejectedCredentialStopsWithoutCarrierWork() {
        val client = live()
        unauthorized = true
        val carrier = FakeCarrier()
        var thrown: Throwable? = null
        try {
            sync(client, carrier).run()
        } catch (error: GatewayAuthException) {
            thrown = error
        }
        assertTrue(thrown is GatewayAuthException)
        assertTrue(carrier.calls.isEmpty())
    }

    private fun assertInvalidCutStaysInBootstrap(highWater: String, count: String) {
        val client = enrollAndUnlock()
        val (command, wire) = desktopCommandEnvelope(SimRoutes.routeId(3), "historical, must not run live")
        events = JSONArray().put(record(1, wire)) // what a live replay from 0 would deliver
        client.captureIncoming(NativeIncomingSms(null, "+15550008888", "must not upload yet", "p-cut-$count", false))
        snapshotCut = highWater to count
        val carrier = FakeCarrier()
        repeat(2) {
            val result = sync(client, carrier).run()
            assertTrue("reported for bounded retry", result.failure is uniffi.peppy_mobile_bindings.MobileBindingsException.InvalidRequest)
            assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        }
        assertTrue("no live replay", requests.none { it.first.startsWith("GET /v1/events") })
        assertEquals("no upload", 0, uploads())
        assertTrue("no OS call", carrier.calls.isEmpty())
        assertEquals(NativePermitState.NOT_RECEIVED, client.beginSendAttempt(command).state)
        assertEquals("0", client.receiveCursor())
    }

    @Test
    fun snapshotCountAboveCoreLimitNeverBypassesBootstrap() {
        val client = enrollAndUnlock()
        snapshotCut = "1000001" to "1000001"
        var now = 1_000L

        val first = sync(client, nowMs = { now }).run()
        now += SIX_HOURS_MS - 1
        val second = sync(client, nowMs = { now }).run()

        assertNull(first.failure)
        assertNull(second.failure)
        assertFalse(first.more)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)
        assertEquals(HistoryTooLargeKind.SNAPSHOT_BEGIN, GatewayStateStore(context).historyTooLargeKind)
        assertNull(client.snapshotProgress())
        assertEquals(0, uploads())
        assertTrue(requests.none { it.first.startsWith("GET /v1/snapshot/records") || it.first.startsWith("GET /v1/events") })
        // Each pass runs the compaction probe (2 fetches); only the first pass also fetches the cut for begin (+1).
        assertEquals("the second pass inside six hours does not begin", 3, requests.count { it.first == "GET /v1/snapshot" })
    }

    @Test
    fun snapshotCountAboveFormerCeilingIsAcceptedAndStagingStarts() {
        val client = enrollAndUnlock()
        snapshotRecords(400)
        snapshotCut = "100001" to "100001"
        var elapsed = 0L
        var stagedPages = 0
        val pagedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also {
                    if (++stagedPages == 1) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }

        val result = sync(pagedClient, elapsedMs = { elapsed }).run()

        assertNull(result.failure)
        assertTrue(result.more)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertFalse(GatewayStateStore(context).hasHistoryTooLarge)
        assertNotNull(client.snapshotProgress())
        assertTrue(client.snapshotProgress()!!.receivedRecords > 0uL)
        assertEquals(1, requests.count { it.first.startsWith("GET /v1/snapshot/records") })
    }

    @Test
    fun tooLargeMarkerIsRetriedOnceAfterSixHoursAndClearedBySuccessfulBegin() {
        val client = enrollAndUnlock()
        snapshotCut = "1000001" to "1000001"
        var now = 1_000L
        sync(client, nowMs = { now }).run()
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)

        snapshotCut = null
        twoSnapshotRecords()
        now += SIX_HOURS_MS - 1
        val skipped = sync(client, nowMs = { now }).run()
        assertNull(skipped.failure)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertTrue("still inside the six-hour window", GatewayStateStore(context).hasHistoryTooLarge)

        now += 1
        val retried = sync(client, nowMs = { now }).run()

        assertNull(retried.failure)
        assertFalse(GatewayStateStore(context).hasHistoryTooLarge)
        assertEquals(SyncPhase.LIVE, state.phase)
    }

    @Test
    fun tooLargeResyncBeginFromReceiveIsNotAFailure() {
        val client = live()
        eventsStatus = 409
        snapshotCut = "1000001" to "1000001"

        val result = sync(client).run()

        assertNull(result.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)
        assertTrue(requests.none { it.first.startsWith("GET /v1/snapshot/records") })
    }

    @Test
    fun tooLargeContactRepairBeginIsNotAFailureAndKeepsTheRepairLatch() {
        val repairing = live()
        repairing.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true
        val refusing = object : NativeClientInterface by repairing {
            override fun beginSnapshotWithCompaction(
                highWater: String,
                recordCount: ULong,
                purpose: NativeSnapshotPurpose,
                serverCompactionGeneration: String?,
            ): NativeSnapshotProgress = throw MobileBindingsException.SnapshotTooLarge()
        }

        val result = sync(refusing).run()

        assertNull(result.failure)
        assertTrue(repairing.contactRepairRequired())
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)
    }

    @Test
    fun tooLargeRecordsPageAppendIsNotAFailureAndNeverGoesLive() {
        val client = enrollAndUnlock()
        twoSnapshotRecords()
        val refusing = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>): NativeSnapshotProgress =
                throw MobileBindingsException.SnapshotTooLarge()
        }

        val result = sync(refusing).run()

        assertNull(result.failure)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)
        assertEquals(HistoryTooLargeKind.DOWNLOAD_BYTE_CAP, GatewayStateStore(context).historyTooLargeKind)
        assertEquals(0, uploads())
    }

    @Test
    fun tooLargeManifestPageAppendIsNotAFailureAndNeverGoesLive() {
        val client = enrollAndUnlock()
        snapshotManifestSupported = true
        twoSnapshotRecords()
        val refusing = object : NativeClientInterface by client {
            override fun appendSnapshotManifest(
                generation: ULong,
                entries: List<NativeSnapshotManifestEntry>,
            ): NativeSnapshotManifestProgress = throw MobileBindingsException.SnapshotTooLarge()
        }

        val result = sync(refusing).run()

        assertNull(result.failure)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)
        assertEquals(0, uploads())
    }

    @Test
    fun capabilityBodyBootstrapsAsImportingAndGoesReadyInTheSamePassThatGoesLive() {
        val client = enrollAndUnlock()
        twoSnapshotRecords()

        val result = sync(client).run()

        assertNull(result.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertEquals(listOf(importingReport(), readyReport()), capabilityBodies())
    }

    @Test
    fun capabilityBodyDuringRecoveryPublishesNoRoutesAndNoSetupState() {
        val client = enrollAndUnlock()
        state.requireRecovery(RecoveryReason.REUSED_DEVICE_IDENTITY)

        sync(client).run()

        assertEquals(
            listOf(SimRoutes.capabilityReport(emptyList(), sendPermissionGranted = false).toString()),
            capabilityBodies(),
        )
    }

    @Test
    fun failedBootstrapPhaseRecordsStableCoreErrorKind() {
        val client = enrollAndUnlock()
        snapshotCut = "2" to "5"
        val traces = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(client).run() }

        val failed = traces.map(::traceFields).single {
            it["event"] == SyncTraceEvent.GATEWAY_PHASE.code && it[SyncTraceField.SUCCESS.code.toString()] == 0L
        }
        assertEquals(4L, failed.getValue(SyncTraceField.PHASE.code.toString()))
        assertEquals("InvalidRequest is core kind 3", 3L, failed.getValue(SyncTraceField.CORE_ERROR.code.toString()))
    }

    @Test
    fun manifestMissingRunStopsAtBudgetAndResumesWithoutRedownloadingStagedRows() {
        val client = enrollAndUnlock()
        snapshotManifestSupported = true
        snapshotRecords(1_000)
        var elapsed = 0L
        var pages = 0
        val timedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also {
                    if (++pages == 1) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }

        val first = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(first.failure)
        assertTrue(first.more)
        assertEquals(SyncPhase.BOOTSTRAP, state.phase)
        assertEquals("200", client.snapshotProgress()?.lastCursor)
        assertEquals(1, requests.count { it.first.startsWith("GET /v1/snapshot/records?high_water=1000&after=0&") })

        elapsed = 0L
        val second = sync(timedClient, elapsedMs = { elapsed }).run()

        assertNull(second.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertEquals(
            "the resumed pass starts after the staged cursor and never downloads cursor 1..200 again",
            1,
            requests.count { it.first.startsWith("GET /v1/snapshot/records") && it.first.contains("&after=0&") },
        )
    }

    @Test
    fun cancellationBetweenManifestMissingRequestsStopsThePass() {
        val client = enrollAndUnlock()
        snapshotManifestSupported = true
        snapshotRecords(1_000)
        var pages = 0
        val cancellingClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also { pages++ }
        }

        assertThrows(CancellationException::class.java) {
            sync(cancellingClient, effectPermitted = { pages == 0 }).run()
        }
        assertEquals(1, requests.count { it.first.startsWith("GET /v1/snapshot/records") })
    }

    @Test
    fun rejectedRestartOfAnExistingGenerationStopsStagingUntilTheRecheckWindow() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)
        var now = 1_000L
        var elapsed = 0L
        var pages = 0
        val timedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also {
                    if (++pages == 1) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }
        // Pass 1 stages one page and stops at the budget, leaving an existing generation.
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        assertEquals("200", client.snapshotProgress()?.lastCursor)

        // Pass 2: the staged generation's next page is refused, so the restart is a begin above the ceiling.
        snapshotCut = "1000001" to "1000001"
        brokenRecordPages.add(LoopbackHttpServer.Response(409))
        elapsed = 0L
        now += 1
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        val markedAt = GatewayStateStore(context).historyTooLargeCheckedMs
        assertEquals(now, markedAt)

        // Pass 3 inside six hours: no staging request, no begin, and the marker does not move.
        val stagingRequestsBefore = requests.count { it.first.startsWith("GET /v1/snapshot/records") || it.first.startsWith("GET /v1/snapshot/manifest") }
        val cutFetchesBefore = requests.count { it.first == "GET /v1/snapshot" }
        now = markedAt!! + SIX_HOURS_MS - 1
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        assertEquals(stagingRequestsBefore, requests.count { it.first.startsWith("GET /v1/snapshot/records") || it.first.startsWith("GET /v1/snapshot/manifest") })
        assertEquals("only the compaction probe fetches the cut", cutFetchesBefore + 1, requests.count { it.first == "GET /v1/snapshot" })
        assertEquals("a skipped pass does not move the marker", markedAt, GatewayStateStore(context).historyTooLargeCheckedMs)

        // Pass 4 after six hours: exactly one attempt (probe plus one cut fetch), which refreshes the marker.
        brokenRecordPages.add(LoopbackHttpServer.Response(409))
        now = markedAt + SIX_HOURS_MS
        val cutFetchesBeforeAttempt = requests.count { it.first == "GET /v1/snapshot" }
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        assertEquals("one attempt: the probe and one cut fetch", cutFetchesBeforeAttempt + 2, requests.count { it.first == "GET /v1/snapshot" })
        assertEquals(now, GatewayStateStore(context).historyTooLargeCheckedMs)
    }

    @Test
    fun byteCapMarkerSkipsSixHoursAndRetriesAfterSevenDays() {
        val client = enrollAndUnlock()
        twoSnapshotRecords()
        var appends = 0
        val refusingAppend = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>): NativeSnapshotProgress {
                appends++
                throw MobileBindingsException.SnapshotTooLarge()
            }
        }
        var now = 1_000L
        sync(refusingAppend, nowMs = { now }).run()
        assertEquals(1, appends)
        assertEquals(HistoryTooLargeKind.DOWNLOAD_BYTE_CAP, GatewayStateStore(context).historyTooLargeKind)

        now += SIX_HOURS_MS
        sync(refusingAppend, nowMs = { now }).run()
        assertEquals("the byte-cap kind does not retry at six hours", 1, appends)

        now = 1_000L + SEVEN_DAYS_MS
        sync(refusingAppend, nowMs = { now }).run()
        assertEquals("one retry after seven days", 2, appends)
    }

    /** Raw persisted setup phase, so the check does not depend on the store's own LIVE read guard. */
    private fun rawPersistedSetupPhase(): String? =
        context.getSharedPreferences("peppy-gateway-state", android.content.Context.MODE_PRIVATE).getString("setup-phase.v1", null)

    @Test
    fun liveResyncPersistsNoSetupProgressAndShowsNoCard() {
        val client = live()
        eventsStatus = 409
        twoSnapshotRecords()

        val result = sync(client).run()

        assertNull(result.failure)
        assertEquals(SyncPhase.LIVE, state.phase)
        assertNull("a LIVE resync must not persist import progress", rawPersistedSetupPhase())
        assertNull(state.setupProgress)
    }

    @Test
    fun liveContactRepairPersistsNoSetupProgress() {
        val repairing = live()
        repairing.requestContactRepair()
        snapshotCompactionSupported = true
        snapshotCompactionActive = true

        val result = sync(repairing).run()

        assertNull(result.failure)
        assertFalse(repairing.contactRepairRequired())
        assertNull("contact repair is not an import", rawPersistedSetupPhase())
    }

    @Test
    fun retainedGenerationCompletingAfterTheWindowEndsTheTooLargeState() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)
        var now = 1_000L
        var elapsed = 0L
        var pages = 0
        val timedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also {
                    if (++pages == 1) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        snapshotCut = "1000001" to "1000001"
        brokenRecordPages.add(LoopbackHttpServer.Response(409))
        elapsed = 0L
        now += 1
        sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()
        assertTrue(GatewayStateStore(context).hasHistoryTooLarge)

        // The window expires; the retained generation is still there and no begin is needed to finish it.
        snapshotCut = null
        now = (GatewayStateStore(context).historyTooLargeCheckedMs ?: error("marker")) + SIX_HOURS_MS
        val result = sync(timedClient, elapsedMs = { elapsed }, nowMs = { now }).run()

        assertNull(result.failure)
        assertEquals("the resumed generation completes and the phone goes LIVE", SyncPhase.LIVE, state.phase)
        assertFalse("completing the retained generation ends the too-large state", GatewayStateStore(context).hasHistoryTooLarge)
    }

    @Test
    fun recordsPageByteCapRefusalKeepsCoreErrorKindWithoutAFailure() {
        val client = enrollAndUnlock()
        twoSnapshotRecords()
        val refusing = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>): NativeSnapshotProgress =
                throw MobileBindingsException.SnapshotTooLarge()
        }
        val traces = mutableListOf<String>()

        val result = SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(refusing).run() }

        assertNull(result.failure)
        assertFalse(result.more)
        val bootstrap = traces.map(::traceFields).single {
            it["event"] == SyncTraceEvent.GATEWAY_PHASE.code && it[SyncTraceField.PHASE.code.toString()] == 4L
        }
        assertEquals(0L, bootstrap.getValue(SyncTraceField.SUCCESS.code.toString()))
        assertEquals(5L, bootstrap.getValue(SyncTraceField.CORE_ERROR.code.toString()))
    }

    @Test
    fun manifestPageByteCapRefusalKeepsCoreErrorKindWithoutAFailure() {
        val client = enrollAndUnlock()
        snapshotManifestSupported = true
        twoSnapshotRecords()
        val refusing = object : NativeClientInterface by client {
            override fun appendSnapshotManifest(
                generation: ULong,
                entries: List<NativeSnapshotManifestEntry>,
            ): NativeSnapshotManifestProgress = throw MobileBindingsException.SnapshotTooLarge()
        }
        val traces = mutableListOf<String>()

        val result = SyncDiagnostics.withTestConfiguration(true, { 10L }, { 2L }, traces::add) { sync(refusing).run() }

        assertNull(result.failure)
        assertFalse(result.more)
        val bootstrap = traces.map(::traceFields).single {
            it["event"] == SyncTraceEvent.GATEWAY_PHASE.code && it[SyncTraceField.PHASE.code.toString()] == 4L
        }
        assertEquals(0L, bootstrap.getValue(SyncTraceField.SUCCESS.code.toString()))
        assertEquals(5L, bootstrap.getValue(SyncTraceField.CORE_ERROR.code.toString()))
    }

    @Test
    fun setupProgressWritesAreThrottledButEndWithTheLastValue() {
        val client = enrollAndUnlock()
        snapshotRecords(3_000)
        var elapsed = 0L
        var pages = 0
        val timedClient = object : NativeClientInterface by client {
            override fun appendSnapshotRawPage(generation: ULong, records: List<NativeRawSnapshotRecord>) =
                client.appendSnapshotRawPage(generation, records).also {
                    // Three pages land within one write interval; the budget then ends the pass.
                    if (++pages == 3) elapsed = GatewaySync.SNAPSHOT_STAGE_BUDGET_MS + 1
                }
        }

        sync(timedClient, elapsedMs = { elapsed }).run()

        assertEquals(
            "the last staged value is written at the end of the phase",
            GatewayStateStore.SetupProgressState(GatewayStateStore.SetupPhase.DOWNLOADING, staged = 600, expected = 3_000),
            state.setupProgress,
        )
    }

    @Test
    fun snapshotCountAboveHighWaterNeverBypassesBootstrap() = assertInvalidCutStaysInBootstrap("2", "5")
}
