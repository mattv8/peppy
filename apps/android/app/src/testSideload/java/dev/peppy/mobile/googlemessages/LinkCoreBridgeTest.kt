package dev.peppy.mobile.googlemessages

import android.os.Looper
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.impl.WorkManagerImpl
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.NativeGateway
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.NativeRcsEligibilityPublication
import uniffi.peppy_mobile_bindings.NativeRcsLinkOwnershipClaimOutcome
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.gatewayPublishRcsEligibility
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMapping

/**
 * Runs the real bridge against the real generated core and host SQLCipher store. Durable outcomes
 * (persisted lifecycle, owner-unlink completion, cutover alerts, WorkManager gateway passes) are
 * asserted through the same core and WorkManager the app uses.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkCoreBridgeTest : GatewayTestBase() {
    private val workers = mutableListOf<Thread>()

    @Before fun initWorkManager() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After fun cleanUp() {
        workers.forEach { worker ->
            worker.join(TimeUnit.SECONDS.toMillis(WAIT_SECONDS))
            assertFalse("worker thread still running", worker.isAlive)
        }
        WorkManagerTestInitHelper.closeWorkDatabase()
        LinkRuntime.resetForTest()
        LinkCoreBridge.resetForTest()
    }

    @Test
    fun `owner unlink invalidation does not wait for a held bridge monitor`() {
        enrollAndUnlock()
        assertEquals(Looper.getMainLooper(), Looper.myLooper())
        val tokenBefore = LinkCoreBridge.observationToken()
        var publicationRecorded = false
        val publication = worker { publicationRecorded = LinkCoreBridge.recordIfNeeded(context, linked()) }
        val invalidationReturned: Boolean
        // The test thread holds the bridge monitor, so the lifecycle publication cannot finish
        // until it releases it. Invalidation runs on another thread, so an invalidation that took
        // the monitor would block here instead of returning.
        synchronized(LinkCoreBridge) {
            publication.start()
            val invalidation = CountDownLatch(1)
            worker {
                LinkCoreBridge.invalidateObservations()
                invalidation.countDown()
            }.start()
            invalidationReturned = invalidation.await(WAIT_SECONDS, TimeUnit.SECONDS)
        }

        assertTrue(invalidationReturned)
        assertEquals(tokenBefore + 1, LinkCoreBridge.observationToken())
        publication.join(TimeUnit.SECONDS.toMillis(WAIT_SECONDS))
        assertTrue(publicationRecorded)
        assertEquals(NativeRcsLinkState.LINKED, persistedLinkState())
    }

    @Test
    fun `stale unlinked observation is dropped by its token once the fence invalidates it`() {
        enrollAndUnlock()
        assertFalse(LinkRuntime.pairingInProgress.get())
        val lease = observationLease()
        val token = LinkCoreBridge.observationToken()
        val pairing = LinkRuntime.tryBeginPairing()!!
        var observationRecorded = false
        var fenceRecorded = false
        val observation = worker { observationRecorded = LinkCoreBridge.recordObservation(token, lease, context, unlinked()) }
        val fence = worker { fenceRecorded = LinkCoreBridge.recordPreStart(context) }
        // Both tasks queue behind the held monitor. Pairing ends before the monitor is released,
        // so the only reason left to drop the observation is the stale token.
        synchronized(LinkCoreBridge) {
            observation.start()
            fence.start()
            awaitObservationInvalidated(token)
            assertTrue(LinkRuntime.endPairing(pairing))
        }
        observation.join(TimeUnit.SECONDS.toMillis(WAIT_SECONDS))
        fence.join(TimeUnit.SECONDS.toMillis(WAIT_SECONDS))

        assertTrue(observationRecorded)
        assertTrue(fenceRecorded)
        // Exactly one durable write: the fence's unknown record. An erroneous Unlinked write would
        // be a second change of state and advance the generation past one.
        assertEquals(NativeRcsLinkState.UNAVAILABLE, persistedLinkState())
        assertEquals(1uL, persistedLinkGeneration())
    }

    @Test
    fun `unlinked observation read before a completed pairing fence is dropped`() {
        enrollAndUnlock()
        val token = LinkCoreBridge.observationToken()
        val lease = observationLease()
        val pairing = LinkRuntime.tryBeginPairing()!!
        assertTrue(LinkCoreBridge.recordPreStart(context))
        assertTrue(LinkRuntime.endPairing(pairing))

        assertTrue(LinkCoreBridge.recordObservation(token, lease, context, unlinked()))

        assertEquals(NativeRcsLinkState.UNAVAILABLE, persistedLinkState())
    }

    @Test
    fun `owner completion publishes unlinked before completing core`() {
        val client = enrollAndUnlock()
        // Owner-unlink candidates exist only after a fresh Unlinked lifecycle and its completion.
        assertTrue(runCatching { client.gatewayRcsOwnerUnlinkCutoverCandidates() }.isFailure)

        assertTrue(LinkCoreBridge.recordExplicitUnlinkCompleted(context))

        assertEquals(NativeRcsLinkState.UNLINKED, persistedLinkState())
        assertTrue(client.gatewayRcsOwnerUnlinkCutoverCandidates().isEmpty())
    }

    @Test
    fun `cutover alert is shown again after reclaim and a second unlink`() {
        val client = enrollAndUnlock()
        val conversationId = UUID.randomUUID().toString()
        val googleConversationId = "google-${UUID.randomUUID()}"
        client.captureIncoming(NativeIncomingSms(conversationId, PEER, "", "link-$conversationId", true))
        linkLifecycle(client)
        claimWithDetector(client, conversationId, googleConversationId, mappingRevision = 1, providerRowId = 11)

        // First incident: the unlink cutover raises the alert, and the owner acknowledges it.
        assertTrue(LinkCoreBridge.recordExplicitUnlinkCompleted(context))
        assertEquals(listOf(conversationId), client.gatewayRcsCutoverAlerts().conversationIds)
        assertTrue(LinkCoreBridge.cutoverMayHaveMissedMessages(context))
        LinkCoreBridge.clearCutoverMayHaveMissedMessages(context)
        assertFalse(LinkCoreBridge.cutoverMayHaveMissedMessages(context))

        // Reclaiming link ownership clears the core's alert code for the conversation.
        linkLifecycle(client)
        claimWithDetector(client, conversationId, googleConversationId, mappingRevision = 2, providerRowId = 12)
        assertTrue(client.gatewayRcsCutoverAlerts().conversationIds.isEmpty())

        // The second unlink cutover recreates the alert for the same conversation. The bridge sweep
        // removes that conversation's acknowledgment, so the same durable ID is shown again.
        assertTrue(LinkCoreBridge.recordExplicitUnlinkCompleted(context))
        assertEquals(listOf(conversationId), client.gatewayRcsCutoverAlerts().conversationIds)
        assertTrue(LinkCoreBridge.cutoverMayHaveMissedMessages(context))
    }

    @Test
    fun `explicit completion without an open gateway is rejected and writes nothing`() {
        assertFalse(LinkCoreBridge.recordExplicitUnlinkCompleted(context))

        // Nothing was written: a later open sees no lifecycle and no owner-unlink completion.
        val client = enrollAndUnlock()
        assertNull(client.gatewayRcsLinkLifecycleGeneration())
        assertTrue(runCatching { client.gatewayRcsOwnerUnlinkCutoverCandidates() }.isFailure)
    }

    @Test
    fun `linked transition enqueues one gateway pass but a linked heartbeat does not`() {
        enrollAndUnlock()

        assertTrue(LinkCoreBridge.recordIfNeeded(context, unlinked()))
        assertEquals(0, activeGatewayWork())
        assertTrue(LinkCoreBridge.recordIfNeeded(context, linked()))
        assertEquals(1, activeGatewayWork())
        cancelGatewayWork()

        assertTrue(LinkCoreBridge.recordIfNeeded(context, linked()))
        assertEquals(0, activeGatewayWork())
    }

    @Test
    fun `changed linked facts are recorded without another gateway pass`() {
        val client = enrollAndUnlock()

        assertTrue(LinkCoreBridge.recordIfNeeded(context, unlinked()))
        assertTrue(LinkCoreBridge.recordIfNeeded(context, linked()))
        assertEquals(1, activeGatewayWork())
        cancelGatewayWork()
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, false, true, 0, "")))

        assertEquals(NativeRcsLinkState.LINKED, persistedLinkState())
        assertEquals(false, client.rcsLinkStatus(deviceId, System.currentTimeMillis())?.health)
        assertEquals(0, activeGatewayWork())
    }

    @Test
    fun `failed lifecycle record without an open gateway does not enqueue a pass`() {
        // No enrolled gateway: the real record returns false, so the transition is not published.
        assertFalse(LinkCoreBridge.recordIfNeeded(context, linked()))
        assertEquals(0, activeGatewayWork())
    }

    @Test
    fun `failed gateway pass enqueue does not fail linking`() {
        enrollAndUnlock()

        // With no WorkManager instance, the enqueue throws. The lifecycle write is already durable.
        withoutWorkManager {
            assertTrue(LinkCoreBridge.recordIfNeeded(context, linked()))
        }

        assertEquals(NativeRcsLinkState.LINKED, persistedLinkState())
        assertEquals(0, activeGatewayWork())
    }

    @Test
    fun `linked observation transition enqueues one gateway pass`() {
        enrollAndUnlock()
        val lease = observationLease()
        val token = LinkCoreBridge.observationToken()

        assertTrue(LinkCoreBridge.recordObservation(token, lease, context, linked()))

        assertEquals(NativeRcsLinkState.LINKED, persistedLinkState())
        assertEquals(1, activeGatewayWork())
    }

    private fun worker(block: () -> Unit): Thread = Thread(block).also { workers += it }

    private fun linked() = LinkStatus(0, "linked", true, true, true, 1, "")

    private fun unlinked() = LinkStatus(0, "unlinked", false, false, false, 0, "")

    private fun persistedLinkState(): NativeRcsLinkState? =
        NativeGateway.open(context)?.rcsLinkStatus(deviceId, System.currentTimeMillis())?.state

    private fun persistedLinkGeneration(): ULong? = NativeGateway.open(context)?.gatewayRcsLinkLifecycleGeneration()

    private fun awaitObservationInvalidated(token: Long) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(WAIT_SECONDS)
        while (LinkCoreBridge.observationToken() == token) {
            check(System.nanoTime() < deadline) { "fence never invalidated observations" }
            Thread.yield()
        }
    }

    private fun activeGatewayWork() = WorkManager.getInstance(context)
        .getWorkInfosForUniqueWork(GatewayWork.NAME).get(WAIT_SECONDS, TimeUnit.SECONDS)
        .count { !it.state.isFinished }

    private fun cancelGatewayWork() {
        WorkManager.getInstance(context).cancelUniqueWork(GatewayWork.NAME).result.get(WAIT_SECONDS, TimeUnit.SECONDS)
    }

    /** Runs [block] with no WorkManager instance, as when WorkManager never initialized. */
    private fun withoutWorkManager(block: () -> Unit) {
        val previous = WorkManagerImpl.getInstance()
        WorkManagerImpl.setDelegate(null)
        try {
            block()
        } finally {
            WorkManagerImpl.setDelegate(previous)
        }
    }

    /** Records a linked lifecycle through the real core, as the link publishes one. */
    private fun linkLifecycle(client: NativeClient) {
        val now = System.currentTimeMillis()
        client.recordRcsLinkLifecycle(NativeRcsLinkState.LINKED, true, true, true, now, now)
    }

    /**
     * Verifies a mapping at [mappingRevision], claims [conversationId] for the link, and records one
     * provider detector. The detector makes the next owner-unlink cutover raise an ambiguous alert.
     * Returns the ownership generation.
     */
    private fun claimWithDetector(
        client: NativeClient,
        conversationId: String,
        googleConversationId: String,
        mappingRevision: Long,
        providerRowId: Long,
    ): ULong {
        val now = System.currentTimeMillis()
        assertEquals(
            NativeRcsMappingVerification.MAPPED,
            gatewayVerifyRcsMapping(client, rcsObservation(googleConversationId, mappingRevision, now), now),
        )
        gatewayPublishRcsEligibility(client, listOf(NativeRcsEligibilityPublication(conversationId)), now)
        // Revision and generations are the verified observation's own values, which the core stored as current.
        val claim = JSONObject()
            .put("conversationId", conversationId)
            .put("googleConversationId", googleConversationId)
            .put("mappingRevision", mappingRevision)
            .put("providerFloor", 0)
            .put("expectedLifecycleGeneration", checkNotNull(client.gatewayRcsLinkLifecycleGeneration()).toLong())
            .put("expectedLinkGeneration", 1)
            .put("expectedSessionGeneration", 1)
            .put("expectedContextRevision", 1)
        val ownership = client.gatewayClaimRcsLinkOwnershipJson(claim.toString())
        assertEquals(NativeRcsLinkOwnershipClaimOutcome.CLAIMED, ownership.outcome)
        val detector = JSONObject()
            .put("providerRowId", providerRowId)
            .put("providerThreadId", 2)
            .put("providerType", 132)
            .put("observedAtMs", now)
            .put("observedGeneration", ownership.ownershipGeneration.toLong())
        assertEquals(
            "recorded",
            client.gatewayRecordRcsProviderDetectorJson(conversationId, ownership.ownershipGeneration, detector.toString()),
        )
        return ownership.ownershipGeneration
    }

    private fun rcsObservation(googleConversationId: String, mappingRevision: Long, now: Long): String {
        val outgoing = JSONObject()
            .put("outgoing_id", "google-outgoing")
            .put("sim_participant_id", "google-outgoing")
            .put("own_identity_raw", OWN_NUMBER)
            .put("sim_payload", JSONObject().put("two", 2).put("sim_number", 7))
            .put("rcs_chats_enabled", true)
        return JSONObject()
            .put("google_conversation_id", googleConversationId)
            .put("google_candidate_count", 1)
            .put("participants", JSONArray().put(JSONObject()
                .put("identity", PEER)
                .put("google_participant_id", "google-peer")
                .put("identifier_type", 1)
                .put("is_visible", true)
                .put("is_me", false)))
            .put("conversation_type", "Rcs")
            .put("status", "eligible")
            .put("send_mode", "Automatic")
            .put("read_only", false)
            .put("default_outgoing", true)
            .put("selected_sim_id", "android-subscription-7")
            .put("selected_sim_own_identity", OWN_NUMBER)
            .put("carrier_binding", JSONObject()
                .put("carrier_route_id", "android-subscription-7")
                .put("android_subscription_id", 7)
                .put("is_current_default_sms", true)
                .put("own_number_raw", OWN_NUMBER)
                .put("own_number_source", "SubscriptionManagerPhoneNumber")
                .put("route_observed_at_ms", now)
                .put("route_context_revision", 1)
                .put("same_handset_evidence_id", "verified-handset"))
            .put("outgoing", outgoing)
            .put("native_evidence", JSONObject()
                .put("lifetime", JSONObject()
                    .put("runtime_id", "runtime")
                    .put("run_generation", 1)
                    .put("link_generation", 1)
                    .put("session_generation", 1)
                    .put("context_revision", 1))
                .put("lookup_id", "lookup")
                .put("lookup_complete", true)
                .put("google_candidate_count", 1)
                .put("settings_revision", 1)
                .put("settings_provenance", "CurrentEvent")
                .put("settings_observed_at_ms", now)
                .put("settings_stream_epoch", 1)
                .put("settings_currentness", "ContinuousVerifiedFeed")
                .put("current_settings_sims", JSONArray().put(outgoing))
                .put("settings_validated_at_ms", now)
                .put("conversation_observed_at_ms", now))
            .put("identity_region", "US")
            .put("observed_at_ms", now)
            .put("context_revision", 1)
            .put("link_generation", 1)
            .put("session_generation", 1)
            .put("canonicalizer_revision", uniffi.peppy_mobile_bindings.rcsCanonicalizerRevision().toLong())
            .put("mapping_revision", mappingRevision)
            .put("link_state", "Linked")
            .toString()
    }

    private fun observationLease(): LinkHandleLease {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost) = object : LinkHandle {
                override fun pair(cookiesJson: ByteArray) = Unit
                override fun relink() = Unit
                override fun run() = Unit
                override fun checkHealth() = 0L
                override fun unlink() = Unit
                override fun stop() = Unit
                override fun close() = Unit
                override fun status() = "{}"
            }
        }
        LinkRuntimeTestHooks.storeFactory = { object : LinkStateStorage {
            override fun read() = ByteArray(0)
            override fun write(state: ByteArray) = Unit
            override fun wipe() = Unit
        } }
        return LinkRuntime.handleLease(context)!!
    }

    private companion object {
        const val PEER = "+12025550123"
        const val OWN_NUMBER = "+12025550101"
        const val WAIT_SECONDS = 5L
    }
}
