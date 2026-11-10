package dev.peppy.mobile.googlemessages

internal data class RcsNativeCommand(val commandId: String, val conversationId: String, val recipient: String)

internal enum class RcsNativeOwnNumberSource { SubscriptionManagerPhoneNumber, TelephonyLine1Number }

internal data class RcsNativeRouteEvidence(
    val routeId: String,
    val subscriptionId: Int,
    val ownNumberRaw: String,
    val ownNumberSource: RcsNativeOwnNumberSource,
    val identityRegion: String,
    val observedAtMs: Long,
)

internal fun interface RcsNativeRouteReader { fun currentRoute(): RcsNativeRouteEvidence? }
internal fun interface RcsNativeClock { fun wallTimeMs(): Long }

/** Arms a ticket-bound revoker until the coordinator's final cleanup. */
internal fun interface RcsNativeRouteChangeRevoker {
    fun register(boundRoute: RcsNativeRouteEvidence, revokeTicket: () -> Unit): AutoCloseable
}

internal data class RcsNativePrepared(
    val ticketId: String,
    val candidateSnapshotJson: String,
    val runIncarnation: String,
    val runGeneration: ULong,
    val streamEpoch: ULong,
    val pairingEvidenceId: String,
)

internal data class RcsNativeContextTuple(val linkGeneration: ULong, val sessionGeneration: ULong, val contextRevision: ULong)

internal sealed interface RcsNativeTarget {
    data class Found(val googleConversationId: String, val snapshotToken: String) : RcsNativeTarget
    data object CarrierOnly : RcsNativeTarget
    data object Unknown : RcsNativeTarget
    data object Ambiguous : RcsNativeTarget
    data object Unsupported : RcsNativeTarget
}

internal sealed interface RcsNativeMappingVerification {
    data object Mapped : RcsNativeMappingVerification
    data object GoogleConversationAmbiguous : RcsNativeMappingVerification
    data object Refused : RcsNativeMappingVerification
}

internal sealed interface RcsNativeCommitResult {
    data object Accepted : RcsNativeCommitResult
    data object Rejected : RcsNativeCommitResult
    /** Go saw a roster change before SendMessage: nothing was sent. */
    data object MembersChanged : RcsNativeCommitResult
    data object OutcomeUnknown : RcsNativeCommitResult
}

/** Group re-proof of one fresh target observation. Proven stores the observation time as the proof time. */
internal enum class RcsNativeGroupVerification { Proven, MembersChanged, Refused }

/** Core refused a group send because the roster or member revision changed. Nothing was sent. */
internal class RcsNativeMembersChanged : IllegalStateException("group members changed")

internal sealed interface RcsNativeRevokeResult {
    data object Revoked : RcsNativeRevokeResult
    data object Late : RcsNativeRevokeResult
    data object None : RcsNativeRevokeResult
}

internal interface RcsNativeGoOperation {
    fun ownerGeneration(): Long
    fun isCurrent(): Boolean
    /** [expectedMemberDigest] is set only for a Live group target; Go rejects a group prepare without it. */
    fun prepare(command: RcsNativeCommand, canonicalizerRevision: Int, route: RcsNativeRouteEvidence, expectedMemberDigest: String?): RcsNativePrepared
    fun bindRoute(ticketId: String, tuple: RcsNativeContextTuple, mappingRevision: ULong, observedAtMs: Long): String
    fun classifyTarget(ticketId: String, classify: (String) -> RcsNativeTarget): RcsNativeTarget?
    fun bindTarget(ticketId: String, googleConversationId: String, snapshotToken: String): String
    fun bindFacts(ticketId: String, factsJson: String): Boolean
    fun commit(ticketId: String, exactCommitJson: String): RcsNativeCommitResult
    /** Must not take a Kotlin monitor, operation lock, or revalidate the lease. */
    fun revoke(ticketId: String): RcsNativeRevokeResult
}

internal open class RcsNativePreparation(
    val factsJson: String,
    private val beginOperation: (String, Long) -> RcsNativeBeginResult,
) : AutoCloseable {
    fun begin(exactObservationJson: String, nowMs: Long): RcsNativeBeginResult = beginOperation(exactObservationJson, nowMs)
    override fun close() = Unit
}

internal sealed interface RcsNativeBeginResult {
    data object KnownRefused : RcsNativeBeginResult
    data class Committed(val handle: RcsNativeCommittedHandle) : RcsNativeBeginResult
    /** Core refused the group begin because the roster changed. Core paused the group; nothing was admitted. */
    data object MembersChanged : RcsNativeBeginResult
    data object DurabilityUnknown : RcsNativeBeginResult
}

internal open class RcsNativeCommittedHandle(
    val exactCommitJson: String,
    val tmpId: String,
    val googleConversationId: String,
) : AutoCloseable {
    override fun close() = Unit
}

internal interface RcsNativeCoreLease : AutoCloseable {
    fun nextContextTuple(runIncarnation: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String): RcsNativeContextTuple
    fun nextMappingRevision(conversationId: String): ULong
    fun verifyMapping(exactObservationJson: String): RcsNativeMappingVerification
    fun classifyTarget(conversationId: String, recipient: String, exactSnapshotJson: String): RcsNativeTarget
    fun prepare(commandId: String, nowMs: Long): RcsNativePreparation
    fun recordTransportAcknowledged(tmpId: String, googleConversationId: String): Boolean
    fun recordSubmitted(tmpId: String, googleConversationId: String): Boolean
    /** [membersChanged] records a group roster refusal; core pauses the group in the same transaction. */
    fun recordPreEffectRefusal(membersChanged: Boolean = false): Boolean
    fun recordOutcomeUnknown(): Boolean = false

    /** The proved member revision of a Live group, or null. */
    fun provedGroupMemberRevision(conversationId: String): ULong? = null
    /** Group route step: advances the link context from the route observation. False when not current. */
    fun verifyGroupRoute(exactObservationJson: String): Boolean = false
    /** Target for a Live group: the owned Google lane and the candidate snapshot token. */
    fun classifyGroupTarget(conversationId: String, exactSnapshotJson: String): RcsNativeTarget = RcsNativeTarget.Unknown
    fun verifyGroupObservation(exactObservationJson: String): RcsNativeGroupVerification = RcsNativeGroupVerification.Refused
}

/** Atomically captures the native owner and registers [revoker] closed over [ticketId]. */
internal fun interface RcsNativeCoreLeaseFactory {
    fun acquire(ticketId: String, revoker: () -> Unit): RcsNativeCoreLease?

    /**
     * The stored member digest of a Live group, read without a lease, as advisory input to the Go prepare.
     * It is not a proof: the route step, re-proof, and freshness-checked preparation still run. Null for a 1:1 or non-Live target.
     */
    fun groupMemberDigest(conversationId: String): String? = null
}

internal enum class RcsNativeSendOutcome { REFUSED, SUBMITTED, CARRIER_ONLY, OUTCOME_UNKNOWN, MEMBERS_CHANGED }
