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

internal data class RcsNativePrepared(
    val ticketId: String,
    val candidateSnapshotJson: String,
    val runIncarnation: String,
    val runGeneration: Long,
    val streamEpoch: Long,
    val pairingEvidenceId: String,
)

internal data class RcsNativeContextTuple(val linkGeneration: Long, val sessionGeneration: Long, val contextRevision: Long)

internal sealed interface RcsNativeTarget {
    data class Found(val googleConversationId: String, val snapshotToken: String) : RcsNativeTarget
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
    data object OutcomeUnknown : RcsNativeCommitResult
}

internal sealed interface RcsNativeRevokeResult {
    data object Revoked : RcsNativeRevokeResult
    data object Late : RcsNativeRevokeResult
    data object None : RcsNativeRevokeResult
}

internal interface RcsNativeGoOperation {
    fun ownerGeneration(): Long
    fun isCurrent(): Boolean
    fun prepare(command: RcsNativeCommand, canonicalizerRevision: Int, route: RcsNativeRouteEvidence): RcsNativePrepared
    fun bindRoute(ticketId: String, tuple: RcsNativeContextTuple, mappingRevision: Long, observedAtMs: Long): String
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
    fun nextContextTuple(runIncarnation: String, runGeneration: Long, streamEpoch: Long, pairingEvidenceId: String): RcsNativeContextTuple
    fun nextMappingRevision(conversationId: String): Long
    fun verifyMapping(exactObservationJson: String): RcsNativeMappingVerification
    fun classifyTarget(conversationId: String, recipient: String, exactSnapshotJson: String): RcsNativeTarget
    fun prepare(commandId: String, nowMs: Long): RcsNativePreparation
    fun recordTransportAcknowledged(tmpId: String, googleConversationId: String): Boolean
    fun recordSubmitted(tmpId: String, googleConversationId: String): Boolean
    fun recordPreEffectRefusal(): Boolean
}

/** Atomically captures the native owner and registers [revoker] closed over [ticketId]. */
internal fun interface RcsNativeCoreLeaseFactory {
    fun acquire(ticketId: String, revoker: () -> Unit): RcsNativeCoreLease?
}

internal enum class RcsNativeSendOutcome { REFUSED, SUBMITTED, OUTCOME_UNKNOWN }
