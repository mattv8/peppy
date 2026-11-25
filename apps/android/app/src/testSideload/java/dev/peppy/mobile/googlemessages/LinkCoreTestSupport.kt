package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsEligibilityPublication
import uniffi.peppy_mobile_bindings.NativeRcsLinkOwnershipClaimOutcome
import uniffi.peppy_mobile_bindings.NativeRcsLinkState
import uniffi.peppy_mobile_bindings.NativeRcsLinkStatus
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.gatewayPublishRcsEligibility
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMapping
import uniffi.peppy_mobile_bindings.rcsCanonicalizerRevision

internal const val LINK_TEST_PEER = "+12025550123"
private const val OWN_NUMBER = "+12025550101"

internal fun GatewayTestBase.persistedLinkState(): NativeRcsLinkState? =
    persistedLinkStatus()?.state

internal fun GatewayTestBase.persistedLinkStatus(): NativeRcsLinkStatus? =
    NativeGateway.open(context)?.rcsLinkStatus(deviceId, System.currentTimeMillis())

internal fun GatewayTestBase.persistedLinkGeneration(): ULong? =
    NativeGateway.open(context)?.gatewayRcsLinkLifecycleGeneration()

/**
 * Verifies a mapping at [mappingRevision], claims [conversationId] for the link, and records one
 * provider detector. The detector makes the next owner-unlink cutover raise an ambiguous alert.
 */
internal fun claimWithDetector(
    client: NativeClient,
    conversationId: String,
    googleConversationId: String,
    mappingRevision: Long,
    providerRowId: Long,
    now: Long = System.currentTimeMillis(),
) {
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
}

internal fun rcsObservation(googleConversationId: String, mappingRevision: Long, now: Long): String {
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
            .put("identity", LINK_TEST_PEER)
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
        .put("canonicalizer_revision", rcsCanonicalizerRevision().toLong())
        .put("mapping_revision", mappingRevision)
        .put("link_state", "Linked")
        .toString()
}

/** Pauses a wipe until the test releases it. */
internal class PausingWipeStore(
    private val wipeEntered: CountDownLatch,
    private val releaseWipe: CountDownLatch,
) : LinkStateStorage {
    override fun read() = ByteArray(0)
    override fun write(state: ByteArray) = Unit
    override fun wipe() {
        wipeEntered.countDown()
        // Longer than the callers' five-second entry waits so a slow wait cannot expire the wipe first.
        assertTrue("wipe was never released", releaseWipe.await(15, TimeUnit.SECONDS))
    }
}
