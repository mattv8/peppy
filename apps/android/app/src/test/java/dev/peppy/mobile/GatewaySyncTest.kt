package dev.peppy.mobile

import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSnapshotProjectionState

/** A loopback stand-in for the server routes the worker uses; no carrier or real server involved. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewaySyncTest : GatewayTestBase() {
    private val requests = mutableListOf<Pair<String, String>>()
    private val requestHeaders = mutableListOf<Map<String, String>>()
    private var events = JSONArray()
    private var snapshot = JSONArray()
    private var eventsStatus = 200
    private var uploadResponse = LoopbackHttpServer.Response(200, """{"cursor":"9","duplicate":false}""")
    /** Responses consumed (one each) before normal snapshot-record pages are served. */
    private val brokenRecordPages = ArrayDeque<LoopbackHttpServer.Response>()
    private var unauthorized = false
    /** Overrides the `/v1/snapshot` cut as (high_water_cursor, record_count). */
    private var snapshotCut: Pair<String, String>? = null
    private var compactionCapabilityResponse = LoopbackHttpServer.Response(204)
    private var capabilitiesResponse = LoopbackHttpServer.Response(204)
    private var retentionCapabilityResponse = LoopbackHttpServer.Response(404)
    private var snapshotCompactionSupported = false
    private var snapshotCompactionActive = false

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
                if (eventsStatus == 200) LoopbackHttpServer.Response(200, JSONObject().put("events", events).toString())
                else LoopbackHttpServer.Response(eventsStatus, """{"code":"resync_required"}""")
            request.method == "POST" && path == "/v1/events" -> uploadResponse
            request.method == "GET" && path == "/v1/snapshot" -> LoopbackHttpServer.Response(
                200,
                (snapshotCut ?: (snapshot.length().toString() to snapshot.length().toString())).let { (highWater, count) ->
                    JSONObject()
                        .put("high_water_cursor", highWater)
                        .put("record_count", count)
                        .put("compaction_supported", snapshotCompactionSupported)
                        .put("compaction_active", snapshotCompactionActive)
                        .apply { if (snapshotCompactionSupported) put("compaction_generation", "1") }
                        .toString()
                },
            )
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

    @After
    fun stop() = server.close()

    private val state get() = GatewayStateStore(context)
    private val routes = SimRoutes.current(3)

    private fun record(cursor: Int, wire: String) = JSONObject().put("cursor", cursor.toString()).put("envelope", JSONObject(wire))

    private fun sync(
        client: NativeClientInterface,
        carrier: FakeCarrier = FakeCarrier(),
        dispatch: () -> DispatchSummary = { SmsDispatcher(context, client, carrier, routes, sendPermissionGranted = true).dispatch() },
        media: () -> MmsTransferResult = { MmsTransferResult(false, 0, 0, emptyList()) },
        contacts: () -> Boolean = { false },
        contactPhotos: () -> Boolean = { false },
        nowMs: () -> Long = { System.currentTimeMillis() },
        sessionCurrent: () -> Boolean = { true },
    ) = GatewaySync(
        client, GatewayHttp(server.origin, TEST_TOKEN), state, deviceId,
        dispatch = dispatch,
        capabilities = SimRoutes.capabilityReport(routes, sendPermissionGranted = true),
        media = media,
        contacts = contacts,
        contactPhotos = contactPhotos,
        sessionCurrent = sessionCurrent,
        nowMs = nowMs,
    )

    private fun uploads() = requests.count { it.first == "POST /v1/events" }

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
        assertEquals(1, requests.count { it.first == "GET /v1/snapshot" })
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
        assertEquals(SimRoutes.capabilityReport(routes, true).toString(), requests.first { it.first == "POST /v1/capabilities" }.second)
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
    fun snapshotCountAboveCoreLimitNeverBypassesBootstrap() = assertInvalidCutStaysInBootstrap("200000", "100001")

    @Test
    fun snapshotCountAboveHighWaterNeverBypassesBootstrap() = assertInvalidCutStaysInBootstrap("2", "5")
}
