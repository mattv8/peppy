package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsCommittedSend
import uniffi.peppy_mobile_bindings.NativeRcsGroupVerification
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerdict
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner
import uniffi.peppy_mobile_bindings.NativeRcsSendPreparation
import uniffi.peppy_mobile_bindings.NativeRcsTargetDecision
import uniffi.peppy_mobile_bindings.gatewayClassifyRcsTarget
import uniffi.peppy_mobile_bindings.gatewayNextRcsContextTuple
import uniffi.peppy_mobile_bindings.gatewayNextRcsMappingRevision
import uniffi.peppy_mobile_bindings.gatewayIssueRcsMediaPermit
import uniffi.peppy_mobile_bindings.gatewayRecordRcsMediaCommit
import uniffi.peppy_mobile_bindings.gatewayRecordRcsMediaOutcome
import uniffi.peppy_mobile_bindings.gatewayPrepareNativeRcsSend
import uniffi.peppy_mobile_bindings.gatewayProvedGroupMemberDigest
import uniffi.peppy_mobile_bindings.gatewayProvedGroupMemberRevision
import uniffi.peppy_mobile_bindings.gatewayRecordRcsPreEffectRefusal
import uniffi.peppy_mobile_bindings.gatewayRecordRcsOutcomeUnknown
import uniffi.peppy_mobile_bindings.gatewayRecordRcsSubmitted
import uniffi.peppy_mobile_bindings.gatewayRecordRcsTransportAck
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsGroupObservation
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsGroupRoute
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMappingVerdict
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsSendMappingVerdict

/** Acquires one exact NativeGateway owner; its client is never resolved again. */
internal class RcsNativeCoreAdapter(
    private val context: Context,
    private val clock: RcsNativeClock,
    private val mediaAttachmentIndex: UInt? = null,
) : RcsNativeCoreLeaseFactory {
    override fun acquire(ticketId: String, revoker: () -> Unit): RcsNativeCoreLease? {
        val gatewayLease = NativeGateway.acquireRcsSendLease(context, ticketId, revoker) ?: return null
        val ordinary = GeneratedNativeRcsCorePort(gatewayLease.client)
        val port = mediaAttachmentIndex?.let { GeneratedNativeRcsMediaPort(gatewayLease.client, ordinary, it) } ?: ordinary
        return NativeRcsCoreLease(port, clock, gatewayLease::close)
    }

    /**
     * Only a Live group has a digest, read without a preparation or freshness check: the stored proof may be
     * older than the send window, and the route step and re-proof refresh it before the freshness-checked prepare.
     */
    override fun groupMemberDigest(conversationId: String): String? {
        val client = NativeGateway.open(context) ?: return null
        return gatewayProvedGroupMemberDigest(client, conversationId)
    }
}

/** Narrow native boundary so behavior can be tested without manufacturing a generated client. */
internal interface NativeRcsCorePort {
    fun nextContextTuple(runtimeId: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String): RcsNativeContextTuple
    fun nextMappingRevision(conversationId: String): ULong
    fun verifyMapping(observationJson: String, nowMs: Long): RcsNativeMappingVerification
    fun verifySendMapping(commandId: String, observationJson: String, nowMs: Long): RcsNativeMappingVerification
    fun classifyTarget(conversationId: String, recipient: String, snapshotJson: String, nowMs: Long): RcsNativeTarget
    fun prepare(commandId: String, nowMs: Long): NativeRcsPreparationPort
    fun recordTransportAcknowledged(commandId: String, ackAtMs: Long)
    fun recordSubmitted(commandId: String)
    fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String, membersChanged: Boolean)
    fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String)
    fun beforeEffect(commandId: String) = Unit
    fun provedGroupMemberRevision(conversationId: String): ULong? = null
    fun verifyGroupRoute(observationJson: String, nowMs: Long): Boolean = false
    fun classifyGroupTarget(conversationId: String, snapshotJson: String): RcsNativeTarget = RcsNativeTarget.Unknown
    fun verifyGroupObservation(observationJson: String, nowMs: Long): RcsNativeGroupVerification = RcsNativeGroupVerification.Refused
}

internal interface NativeRcsPreparationPort : AutoCloseable {
    fun factsJson(): String
    fun begin(observationJson: String, nowMs: Long): NativeRcsCommittedPort
}

internal interface NativeRcsCommittedPort : AutoCloseable {
    fun commandId(): String
    fun commitJson(): String
    fun tmpId(): String
    fun googleConversationId(): String
}

internal class NativeRcsCoreLease(
    private val port: NativeRcsCorePort,
    private val clock: RcsNativeClock,
    private val release: () -> Unit,
) : RcsNativeCoreLease {
    private var closed = false
    private var prepared = false
    private var admitted: Admitted? = null
    private var refusalRecorded = false

    override fun nextContextTuple(runIncarnation: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String): RcsNativeContextTuple {
        requireOpen()
        return port.nextContextTuple(runIncarnation, runGeneration, streamEpoch, pairingEvidenceId)
    }

    override fun nextMappingRevision(conversationId: String): ULong {
        requireOpen()
        return port.nextMappingRevision(conversationId)
    }

    override fun verifyMapping(exactObservationJson: String): RcsNativeMappingVerification {
        requireOpen()
        return port.verifyMapping(exactObservationJson, clock.wallTimeMs())
    }

    override fun verifySendMapping(commandId: String, exactObservationJson: String): RcsNativeMappingVerification {
        requireOpen()
        return port.verifySendMapping(commandId, exactObservationJson, clock.wallTimeMs())
    }

    override fun classifyTarget(conversationId: String, recipient: String, exactSnapshotJson: String): RcsNativeTarget {
        requireOpen()
        return port.classifyTarget(conversationId, recipient, exactSnapshotJson, clock.wallTimeMs())
    }

    override fun prepare(commandId: String, nowMs: Long): RcsNativePreparation {
        requireOpen()
        check(!prepared) { "native RCS preparation already used" }
        prepared = true
        val native = port.prepare(commandId, clock.wallTimeMs())
        val facts = try {
            native.factsJson()
        } catch (failure: Throwable) {
            try { native.close() } catch (closeFailure: Throwable) { failure.addSuppressed(closeFailure) }
            preserveCoreInterruption(failure)
            throw failure
        }
        return ManagedNativeRcsPreparation(facts, NativeRcsPreparationState(native, commandId, this))
    }

    override fun recordTransportAcknowledged(tmpId: String, googleConversationId: String): Boolean = recordForAdmitted(tmpId, googleConversationId) {
        port.recordTransportAcknowledged(it.commandId, clock.wallTimeMs())
    }

    override fun recordSubmitted(tmpId: String, googleConversationId: String): Boolean = recordForAdmitted(tmpId, googleConversationId) {
        port.recordSubmitted(it.commandId)
    }

    override fun provedGroupMemberRevision(conversationId: String): ULong? {
        requireOpen()
        return port.provedGroupMemberRevision(conversationId)
    }

    override fun verifyGroupRoute(exactObservationJson: String): Boolean {
        requireOpen()
        return port.verifyGroupRoute(exactObservationJson, clock.wallTimeMs())
    }

    override fun classifyGroupTarget(conversationId: String, exactSnapshotJson: String): RcsNativeTarget {
        requireOpen()
        return port.classifyGroupTarget(conversationId, exactSnapshotJson)
    }

    override fun verifyGroupObservation(exactObservationJson: String): RcsNativeGroupVerification {
        requireOpen()
        return port.verifyGroupObservation(exactObservationJson, clock.wallTimeMs())
    }

    override fun recordPreEffectRefusal(membersChanged: Boolean): Boolean {
        requireOpen()
        val committed = admitted ?: return false
        if (refusalRecorded) return false
        return try {
            port.recordPreEffectRefusal(committed.commandId, committed.tmpId, committed.googleConversationId, membersChanged)
            refusalRecorded = true
            true
        } catch (failure: Throwable) {
            preserveCoreInterruption(failure)
            false
        }
    }

    override fun recordOutcomeUnknown(): Boolean {
        val committed = admitted ?: return false
        return recordForAdmitted(committed.tmpId, committed.googleConversationId) {
            port.recordOutcomeUnknown(it.commandId, it.tmpId, it.googleConversationId)
        }
    }

    override fun beforeEffect(): Boolean {
        requireOpen()
        val committed = admitted ?: return false
        return try {
            port.beforeEffect(committed.commandId)
            true
        } catch (failure: Throwable) {
            preserveCoreInterruption(failure)
            false
        }
    }

    override fun close() {
        if (closed) return
        closed = true
        release()
    }

    private fun beginCommitted(commandId: String, committed: NativeRcsCommittedPort): RcsNativeBeginResult {
        var committedClosed = false
        fun closeCommitted() {
            if (committedClosed) return
            committedClosed = true
            committed.close()
        }
        return try {
            val admittedCommandId = committed.commandId()
            val commitJson = committed.commitJson()
            val tmpId = committed.tmpId()
            val googleConversationId = committed.googleConversationId()
            if (admittedCommandId != commandId) {
                closeCommitted()
                RcsNativeBeginResult.DurabilityUnknown
            } else {
                admitted = Admitted(admittedCommandId, tmpId, googleConversationId)
                RcsNativeBeginResult.Committed(ManagedNativeRcsCommittedHandle(commitJson, tmpId, googleConversationId, committed))
            }
        } catch (failure: Throwable) {
            try { closeCommitted() } catch (closeFailure: Throwable) { failure.addSuppressed(closeFailure) }
            preserveCoreInterruption(failure)
            RcsNativeBeginResult.DurabilityUnknown
        }
    }

    private fun recordForAdmitted(tmpId: String, googleConversationId: String, operation: (Admitted) -> Unit): Boolean {
        requireOpen()
        val committed = admitted ?: return false
        if (tmpId != committed.tmpId || googleConversationId != committed.googleConversationId) return false
        return try {
            operation(committed)
            true
        } catch (failure: Throwable) {
            preserveCoreInterruption(failure)
            false
        }
    }

    private fun requireOpen() = check(!closed) { "native RCS lease is closed" }

    private data class Admitted(val commandId: String, val tmpId: String, val googleConversationId: String)

    internal class NativeRcsPreparationState(
        private val native: NativeRcsPreparationPort,
        private val commandId: String,
        private val lease: NativeRcsCoreLease,
    ) {
        private var begun = false
        private var closed = false

        fun begin(observationJson: String, _nowMs: Long): RcsNativeBeginResult {
            if (closed) return RcsNativeBeginResult.DurabilityUnknown
            if (begun) return RcsNativeBeginResult.DurabilityUnknown
            begun = true
            return try {
                lease.requireOpen()
                lease.beginCommitted(commandId, native.begin(observationJson, lease.clock.wallTimeMs()))
            } catch (_: MobileBindingsException.GroupMembersChanged) {
                // Core paused the group and admitted nothing; this is a definite refusal, not an unknown outcome.
                RcsNativeBeginResult.MembersChanged
            } catch (_: MobileBindingsException.InvalidRequest) {
                // Binding contract: this begin error is before any committed attempt.
                RcsNativeBeginResult.KnownRefused
            } catch (failure: Throwable) {
                preserveCoreInterruption(failure)
                RcsNativeBeginResult.DurabilityUnknown
            }
        }

        fun close() {
            if (closed) return
            closed = true
            native.close()
        }
    }
}

private class ManagedNativeRcsPreparation(
    factsJson: String,
    private val state: NativeRcsCoreLease.NativeRcsPreparationState,
) : RcsNativePreparation(factsJson, state::begin) {
    override fun close() = state.close()
}

private class ManagedNativeRcsCommittedHandle(
    commitJson: String,
    tmpId: String,
    googleConversationId: String,
    private val native: NativeRcsCommittedPort,
) : RcsNativeCommittedHandle(commitJson, tmpId, googleConversationId) {
    private var closed = false

    override fun close() {
        if (closed) return
        closed = true
        native.close()
    }
}

private class GeneratedNativeRcsCorePort(private val client: NativeClient) : NativeRcsCorePort {
    override fun nextContextTuple(runtimeId: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String) =
        gatewayNextRcsContextTuple(client, runtimeId, runGeneration, streamEpoch, pairingEvidenceId).let {
            RcsNativeContextTuple(it.linkGeneration, it.sessionGeneration, it.contextRevision)
        }

    override fun nextMappingRevision(conversationId: String) = gatewayNextRcsMappingRevision(client, conversationId)

    override fun verifyMapping(observationJson: String, nowMs: Long) =
        recorded(gatewayVerifyRcsMappingVerdict(client, observationJson, nowMs))

    override fun verifySendMapping(commandId: String, observationJson: String, nowMs: Long) =
        recorded(gatewayVerifyRcsSendMappingVerdict(client, observationJson, commandId, nowMs))

    /** Records the core's fixed refusal name, so a refused send can be told apart on the phone. */
    private fun recorded(verdict: NativeRcsMappingVerdict): RcsNativeMappingVerification {
        verdict.refusalCode?.let { recordGoogleMessagesLinkEvent("mapping_refusal", it, 0L) }
        return nativeRcsMappingVerification(verdict.verification)
    }

    override fun classifyTarget(conversationId: String, recipient: String, snapshotJson: String, nowMs: Long) =
        nativeRcsTarget(gatewayClassifyRcsTarget(client, conversationId, recipient, snapshotJson, nowMs))

    override fun prepare(commandId: String, nowMs: Long) = GeneratedNativeRcsPreparationPort(gatewayPrepareNativeRcsSend(client, commandId, nowMs))
    override fun recordTransportAcknowledged(commandId: String, ackAtMs: Long) = gatewayRecordRcsTransportAck(client, commandId, ackAtMs)
    override fun recordSubmitted(commandId: String) = gatewayRecordRcsSubmitted(client, commandId)
    override fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String, membersChanged: Boolean) =
        gatewayRecordRcsPreEffectRefusal(client, commandId, tmpId, googleConversationId, membersChanged, System.currentTimeMillis())
    override fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String) =
        gatewayRecordRcsOutcomeUnknown(client, commandId, tmpId, googleConversationId)

    override fun provedGroupMemberRevision(conversationId: String) =
        gatewayProvedGroupMemberRevision(client, conversationId)?.toULongOrNull()

    override fun classifyGroupTarget(conversationId: String, snapshotJson: String): RcsNativeTarget {
        // The owned group lane names the Google conversation; the candidate snapshot names the token.
        val googleId = client.gatewayRcsLinkOwnedConversations()
            .firstOrNull { it.conversationId == conversationId && it.owner == NativeRcsReceiveOwner.LINK }
            ?.googleConversationId ?: return RcsNativeTarget.Unknown
        val token = JSONObject(snapshotJson).optString("snapshot_token").takeIf(String::isNotEmpty) ?: return RcsNativeTarget.Unknown
        return RcsNativeTarget.Found(googleId, token)
    }

    override fun verifyGroupRoute(observationJson: String, nowMs: Long) = gatewayVerifyRcsGroupRoute(client, observationJson, nowMs)

    override fun verifyGroupObservation(observationJson: String, nowMs: Long) =
        nativeRcsGroupVerification(gatewayVerifyRcsGroupObservation(client, observationJson, nowMs))
}

/** Uses the normal route proof port while replacing only the per-attachment ledger operations. */
private class GeneratedNativeRcsMediaPort(
    private val client: NativeClient,
    private val ordinary: NativeRcsCorePort,
    private val attachmentIndex: UInt,
) : NativeRcsCorePort by ordinary {
    override fun prepare(commandId: String, nowMs: Long) =
        GeneratedNativeRcsPreparationPort(gatewayIssueRcsMediaPermit(client, commandId, attachmentIndex, nowMs))

    override fun recordTransportAcknowledged(commandId: String, ackAtMs: Long) = Unit
    override fun recordSubmitted(commandId: String) =
        gatewayRecordRcsMediaOutcome(client, commandId, attachmentIndex, "ok", System.currentTimeMillis())
    override fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String, membersChanged: Boolean) =
        gatewayRecordRcsMediaOutcome(client, commandId, attachmentIndex, if (membersChanged) "members_changed" else "rejected", System.currentTimeMillis())
    override fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String) =
        gatewayRecordRcsMediaOutcome(client, commandId, attachmentIndex, "outcome_unknown", System.currentTimeMillis())
    override fun beforeEffect(commandId: String) = gatewayRecordRcsMediaCommit(client, commandId, attachmentIndex)
}

internal fun nativeRcsGroupVerification(value: NativeRcsGroupVerification) = when (value) {
    NativeRcsGroupVerification.PROVEN -> RcsNativeGroupVerification.Proven
    NativeRcsGroupVerification.MEMBERS_CHANGED -> RcsNativeGroupVerification.MembersChanged
    NativeRcsGroupVerification.REFUSED -> RcsNativeGroupVerification.Refused
}

internal fun nativeRcsMappingVerification(value: NativeRcsMappingVerification) = when (value) {
    NativeRcsMappingVerification.MAPPED -> RcsNativeMappingVerification.Mapped
    NativeRcsMappingVerification.GOOGLE_CONVERSATION_AMBIGUOUS -> RcsNativeMappingVerification.GoogleConversationAmbiguous
    NativeRcsMappingVerification.REFUSED -> RcsNativeMappingVerification.Refused
}

internal fun nativeRcsTarget(target: NativeRcsTargetDecision): RcsNativeTarget = when (target) {
    is NativeRcsTargetDecision.Found -> RcsNativeTarget.Found(target.googleConversationId, target.snapshotToken)
    NativeRcsTargetDecision.Ambiguous -> RcsNativeTarget.Ambiguous
    NativeRcsTargetDecision.Unknown -> RcsNativeTarget.Unknown
    NativeRcsTargetDecision.NotFoundComplete -> RcsNativeTarget.CarrierOnly
    NativeRcsTargetDecision.Unsupported -> RcsNativeTarget.Unsupported
}

private class GeneratedNativeRcsPreparationPort(private val native: NativeRcsSendPreparation) : NativeRcsPreparationPort {
    override fun factsJson() = native.factsJson()
    override fun begin(observationJson: String, nowMs: Long) = GeneratedNativeRcsCommittedPort(native.begin(observationJson, nowMs))
    override fun close() = native.close()
}

private class GeneratedNativeRcsCommittedPort(private val native: NativeRcsCommittedSend) : NativeRcsCommittedPort {
    override fun commandId() = native.commandId()
    override fun commitJson() = native.commitJson()
    override fun tmpId() = native.tmpId()
    override fun googleConversationId() = native.googleConversationId()
    override fun close() = native.close()
}

private fun preserveCoreInterruption(failure: Throwable) {
    if (failure is InterruptedException) Thread.currentThread().interrupt()
}
