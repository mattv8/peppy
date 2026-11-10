package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsBootstrapResolution
import uniffi.peppy_mobile_bindings.NativeRcsLinkOwnershipClaimOutcome
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.gatewayNextRcsContextTuple
import uniffi.peppy_mobile_bindings.gatewayNextRcsMappingRevision
import uniffi.peppy_mobile_bindings.gatewayResolveRcsBootstrapConversations
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMapping
import uniffi.peppy_mobile_bindings.rcsCanonicalizerRevision

internal interface RcsBootstrapCorePort {
    fun ownedGoogleIds(): Set<String>
    fun resolve(candidates: List<Pair<String, String>>): List<NativeRcsBootstrapResolution>
    fun tuple(snapshot: JSONObject): RcsNativeContextTuple
    fun revision(conversationId: String): ULong
    fun lifecycleGeneration(): ULong?
    fun verify(observation: String, nowMs: Long): NativeRcsMappingVerification
    fun claim(json: String): NativeRcsLinkOwnershipClaimOutcome
}

internal interface RcsBootstrapLinkPort {
    fun candidateSnapshot(): String
    fun observe(request: String): String
}

internal interface RcsBootstrapClock { fun nowMs(): Long }

/**
 * Bounded snapshot-order reconciliation. The Go snapshot exposes `last_message_ms` for recency ordering.
 *
 * A targeted run returns one [RcsBootstrapResult] for its key. Refusals that a retry cannot change are terminal.
 * Every other failure is transient; see [RcsBootstrapResult] for the classification.
 */
internal class RcsLinkBootstrap(
    private val core: RcsBootstrapCorePort,
    private val link: RcsBootstrapLinkPort,
    private val routeReader: RcsNativeRouteReader,
    private val clock: RcsBootstrapClock = object : RcsBootstrapClock { override fun nowMs() = System.currentTimeMillis() },
    private val publish: (Collection<String>) -> Unit = {},
    private val sweep: (String) -> RcsRecoveryResult = { RcsRecoveryResult.ABORTED },
    private val current: () -> Boolean = { true },
) {
    private var continuationGoogleId: String? = null

    constructor(context: Context, lease: LinkHandleLease, current: () -> Boolean = { true }) : this(
        GeneratedRcsBootstrapCorePort(NativeGateway.open(context.applicationContext)),
        object : RcsBootstrapLinkPort {
            override fun candidateSnapshot() = lease.handle.conversationCandidateSnapshot()
            override fun observe(request: String) = lease.handle.bootstrapRcsObservation(request)
        },
        RcsNativeAndroidRouteReader(context, RcsNativeClock(System::currentTimeMillis)),
        publish = { ids ->
            NativeGateway.open(context.applicationContext)?.let { client ->
                RcsEligibilityPublisher(GeneratedRcsEligibilityPublisherPort(client), current).publish(ids)
            }
        },
        sweep = { googleId -> LinkRuntime.resumeRcsReceiveConversation(lease, googleId, AtomicBoolean()) },
        current = current,
    )

    fun run(targetGoogleId: String? = null): RcsBootstrapResult {
        if (!current()) return RcsBootstrapResult.STOPPED
        val snapshot = parseSnapshot() ?: return RcsBootstrapResult.TRANSIENT
        val lifecycleGeneration = runCandidate { core.lifecycleGeneration() } ?: return RcsBootstrapResult.TRANSIENT
        val owned = runCandidate { core.ownedGoogleIds() } ?: return RcsBootstrapResult.TRANSIENT
        if (targetGoogleId != null && targetGoogleId in owned) return sweepConversation(targetGoogleId).toBootstrapResult()
        val candidates = eligibleCandidates(snapshot, targetGoogleId)
        if (targetGoogleId != null && candidates.isEmpty()) return RcsBootstrapResult.TRANSIENT
        val claimed = mutableListOf<String>()
        var transient = false
        var attempted = 0
        var targetResult: RcsBootstrapResult? = null
        val attemptedCandidates = candidates.filter { it.googleId !in owned }.take(MAX_CANDIDATES)
        val resolutions = runCandidate { core.resolve(attemptedCandidates.map { it.number to it.googleId }) }
            ?: return RcsBootstrapResult.TRANSIENT
        for ((candidate, resolution) in attemptedCandidates.zip(resolutions)) {
            if (!current()) return RcsBootstrapResult.STOPPED
            if (attempted == MAX_CANDIDATES) break
            attempted += 1
            continuationGoogleId = candidate.googleId
            val outcome = attempt(candidate, resolution, snapshot, lifecycleGeneration)
            val result = outcome.toBootstrapResult()
            if (result == RcsBootstrapResult.TRANSIENT) transient = true
            if (targetGoogleId != null) targetResult = result
            if (outcome is Attempt.Claimed) claimed += outcome.conversationId
        }
        if (claimed.isNotEmpty()) publish(claimed)
        if (!current()) return RcsBootstrapResult.STOPPED
        if (targetGoogleId == null) return if (transient) RcsBootstrapResult.TRANSIENT else RcsBootstrapResult.COMPLETE
        if (targetResult != RcsBootstrapResult.COMPLETE) recordGoogleMessagesLinkEvent("link", "rcs_bootstrap_failed", 0)
        return targetResult ?: RcsBootstrapResult.STOPPED
    }

    private fun attempt(candidate: Candidate, resolution: NativeRcsBootstrapResolution, snapshot: JSONObject, lifecycleGeneration: ULong): Attempt {
        val conversationId = when (resolution) {
            NativeRcsBootstrapResolution.Uncertain -> return Attempt.Transient
            is NativeRcsBootstrapResolution.Resolved -> resolution.conversationId
            else -> return Attempt.Transient
        }
        val tuple = runCandidate { core.tuple(snapshot) } ?: return Attempt.Transient
        val revision = runCandidate { core.revision(conversationId) } ?: return Attempt.Transient
        val route = runCandidate { routeReader.currentRoute() } ?: return Attempt.Transient
        val proof = runCandidate { observe(candidate.googleId, snapshot, tuple, revision, route) } ?: return Attempt.Transient
        val verification = runCandidate { core.verify(proof, clock.nowMs()) } ?: return Attempt.Transient
        if (verification != NativeRcsMappingVerification.MAPPED) return Attempt.Transient
        val outcome = runCandidate {
            core.claim(claimJson(conversationId, candidate.googleId, revision, tuple, lifecycleGeneration))
        } ?: return Attempt.Transient
        if (outcome != NativeRcsLinkOwnershipClaimOutcome.CLAIMED) return Attempt.Transient
        return Attempt.Claimed(conversationId, sweepConversation(candidate.googleId))
    }

    private fun sweepConversation(googleId: String): RcsRecoveryResult =
        runCatching { sweep(googleId) }.getOrDefault(RcsRecoveryResult.ABORTED)

    private fun parseSnapshot(): JSONObject? = runCandidate { JSONObject(link.candidateSnapshot()) }
        ?.takeIf { it.optString("code") == "conversation_candidate_snapshot" && it.optBoolean("complete") }

    private fun eligibleCandidates(snapshot: JSONObject, target: String?): List<Candidate> {
        val values = snapshot.optJSONArray("conversations") ?: JSONArray()
        val ordered = (0 until values.length()).mapNotNull(values::optJSONObject).mapNotNull(::candidate)
            .sortedByDescending { it.lastMessageMs ?: Long.MIN_VALUE }
        if (target != null) return ordered.filter { it.googleId == target }
        return continuationGoogleId?.let { cursor ->
            val index = ordered.indexOfFirst { it.googleId == cursor }
            if (index >= 0) ordered.drop(index + 1) + ordered.take(index + 1) else ordered
        } ?: ordered
    }

    private fun candidate(value: JSONObject): Candidate? {
        val googleId = value.optString("google_conversation_id").takeIf(String::isNotEmpty) ?: return null
        if (value.optBoolean("group") || value.optString("classification") != "eligible") return null
        val participants = value.optJSONArray("participants") ?: return null
        val visible = (0 until participants.length()).mapNotNull(participants::optJSONObject)
            .filter { it.optBoolean("visible") && !it.optBoolean("self") }
        val number = visible.singleOrNull()?.optString("raw_number")?.takeIf(String::isNotEmpty) ?: return null
        return Candidate(googleId, number, value.optLong("last_message_ms").takeIf { it > 0 })
    }

    /** Returns the Go proof only. */
    private fun observe(googleId: String, snapshot: JSONObject, tuple: RcsNativeContextTuple, revision: ULong, route: RcsNativeRouteEvidence): String {
        val request = JSONObject()
            .put("google_conversation_id", googleId)
            .put("link_generation", tuple.linkGeneration.toLong())
            .put("session_generation", tuple.sessionGeneration.toLong())
            .put("context_revision", tuple.contextRevision.toLong())
            .put("mapping_revision", revision.toLong())
            .put("identity_region", route.identityRegion)
            .put("canonicalizer_revision", rcsCanonicalizerRevision().toInt())
            .put("carrier_binding", route.carrierBindingJson().put("is_current_default_sms", true).put("route_context_revision", tuple.contextRevision.toLong()).put("same_handset_evidence_id", snapshot.optString("pairing_evidence_id")))
        val response = JSONObject(link.observe(request.toString()))
        if (response.optString("code") != "ok") throw IllegalStateException("bootstrap_observation_refused")
        return response.getString("observation")
    }

    private fun claimJson(conversationId: String, googleId: String, revision: ULong, tuple: RcsNativeContextTuple, lifecycleGeneration: ULong) = JSONObject()
        .put("conversationId", conversationId).put("googleConversationId", googleId).put("mappingRevision", revision.toLong())
        .put("providerFloor", 0).put("expectedLifecycleGeneration", lifecycleGeneration.toLong())
        .put("expectedLinkGeneration", tuple.linkGeneration.toLong()).put("expectedSessionGeneration", tuple.sessionGeneration.toLong())
        .put("expectedContextRevision", tuple.contextRevision.toLong()).toString()

    private fun <T> runCandidate(block: () -> T): T? = try { block() } catch (_: Throwable) {
        recordGoogleMessagesLinkEvent("link", "rcs_bootstrap_failed", 0)
        null
    }

    private data class Candidate(val googleId: String, val number: String, val lastMessageMs: Long?)

    private companion object { const val MAX_CANDIDATES = 40 }
}

/** Per-candidate outcome. Refused is a terminal decision; Transient is retried on a later trigger. */
private sealed interface Attempt {
    data class Claimed(val conversationId: String, val recovery: RcsRecoveryResult) : Attempt
    object Transient : Attempt
}

private fun RcsRecoveryResult.toBootstrapResult() =
    if (this == RcsRecoveryResult.COMPLETE) RcsBootstrapResult.COMPLETE else RcsBootstrapResult.TRANSIENT

private fun Attempt.toBootstrapResult(): RcsBootstrapResult = when (this) {
    is Attempt.Claimed -> recovery.toBootstrapResult()
    Attempt.Transient -> RcsBootstrapResult.TRANSIENT
}

private class GeneratedRcsBootstrapCorePort(client: NativeClient?) : RcsBootstrapCorePort {
    private val client = requireNotNull(client)
    override fun ownedGoogleIds() = client.gatewayRcsLinkOwnedConversations().map { it.googleConversationId }.toSet()
    override fun resolve(candidates: List<Pair<String, String>>) = gatewayResolveRcsBootstrapConversations(client, JSONArray().apply {
        candidates.forEach { (identity, googleId) -> put(JSONObject().put("participantIdentity", identity).put("googleConversationId", googleId)) }
    }.toString())
    override fun tuple(snapshot: JSONObject) = gatewayNextRcsContextTuple(client, snapshot.optString("run_incarnation"), snapshot.optLong("generation").toULong(), snapshot.optLong("stream_epoch").toULong(), snapshot.optString("pairing_evidence_id")).let { RcsNativeContextTuple(it.linkGeneration, it.sessionGeneration, it.contextRevision) }
    override fun revision(conversationId: String) = gatewayNextRcsMappingRevision(client, conversationId)
    override fun lifecycleGeneration() = client.gatewayRcsLinkLifecycleGeneration()
    override fun verify(observation: String, nowMs: Long) = gatewayVerifyRcsMapping(client, observation, nowMs)
    override fun claim(json: String) = client.gatewayClaimRcsLinkOwnershipJson(json).outcome
}
