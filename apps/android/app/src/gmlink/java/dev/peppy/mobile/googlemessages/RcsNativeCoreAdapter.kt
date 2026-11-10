package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.gateway.NativeGateway
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsCommittedSend
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.NativeRcsSendPreparation
import uniffi.peppy_mobile_bindings.NativeRcsTargetDecision
import uniffi.peppy_mobile_bindings.gatewayClassifyRcsTarget
import uniffi.peppy_mobile_bindings.gatewayNextRcsContextTuple
import uniffi.peppy_mobile_bindings.gatewayNextRcsMappingRevision
import uniffi.peppy_mobile_bindings.gatewayPrepareNativeRcsSend
import uniffi.peppy_mobile_bindings.gatewayRecordRcsPreEffectRefusal
import uniffi.peppy_mobile_bindings.gatewayRecordRcsOutcomeUnknown
import uniffi.peppy_mobile_bindings.gatewayRecordRcsSubmitted
import uniffi.peppy_mobile_bindings.gatewayRecordRcsTransportAck
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsMapping

/** Acquires one exact NativeGateway owner; its client is never resolved again. */
internal class RcsNativeCoreAdapter(
    private val context: Context,
    private val clock: RcsNativeClock,
) : RcsNativeCoreLeaseFactory {
    override fun acquire(ticketId: String, revoker: () -> Unit): RcsNativeCoreLease? {
        val gatewayLease = NativeGateway.acquireRcsSendLease(context, ticketId, revoker) ?: return null
        return NativeRcsCoreLease(GeneratedNativeRcsCorePort(gatewayLease.client), clock, gatewayLease::close)
    }
}

/** Narrow native boundary so behavior can be tested without manufacturing a generated client. */
internal interface NativeRcsCorePort {
    fun nextContextTuple(runtimeId: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String): RcsNativeContextTuple
    fun nextMappingRevision(conversationId: String): ULong
    fun verifyMapping(observationJson: String, nowMs: Long): RcsNativeMappingVerification
    fun classifyTarget(conversationId: String, recipient: String, snapshotJson: String, nowMs: Long): RcsNativeTarget
    fun prepare(commandId: String, nowMs: Long): NativeRcsPreparationPort
    fun recordTransportAcknowledged(commandId: String, ackAtMs: Long)
    fun recordSubmitted(commandId: String)
    fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String)
    fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String)
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

    override fun recordPreEffectRefusal(): Boolean {
        requireOpen()
        val committed = admitted ?: return false
        if (refusalRecorded) return false
        return try {
            port.recordPreEffectRefusal(committed.commandId, committed.tmpId, committed.googleConversationId)
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

    override fun verifyMapping(observationJson: String, nowMs: Long) = nativeRcsMappingVerification(gatewayVerifyRcsMapping(client, observationJson, nowMs))

    override fun classifyTarget(conversationId: String, recipient: String, snapshotJson: String, nowMs: Long) =
        nativeRcsTarget(gatewayClassifyRcsTarget(client, conversationId, recipient, snapshotJson, nowMs))

    override fun prepare(commandId: String, nowMs: Long) = GeneratedNativeRcsPreparationPort(gatewayPrepareNativeRcsSend(client, commandId, nowMs))
    override fun recordTransportAcknowledged(commandId: String, ackAtMs: Long) = gatewayRecordRcsTransportAck(client, commandId, ackAtMs)
    override fun recordSubmitted(commandId: String) = gatewayRecordRcsSubmitted(client, commandId)
    override fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String) =
        gatewayRecordRcsPreEffectRefusal(client, commandId, tmpId, googleConversationId)
    override fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String) =
        gatewayRecordRcsOutcomeUnknown(client, commandId, tmpId, googleConversationId)
}

internal fun nativeRcsMappingVerification(value: NativeRcsMappingVerification) = when (value) {
    NativeRcsMappingVerification.MAPPED -> RcsNativeMappingVerification.Mapped
    NativeRcsMappingVerification.GOOGLE_CONVERSATION_AMBIGUOUS -> RcsNativeMappingVerification.GoogleConversationAmbiguous
    NativeRcsMappingVerification.REFUSED -> RcsNativeMappingVerification.Refused
}

internal fun nativeRcsTarget(target: NativeRcsTargetDecision): RcsNativeTarget = when (target) {
    is NativeRcsTargetDecision.Found -> RcsNativeTarget.Found(target.googleConversationId, target.snapshotToken)
    NativeRcsTargetDecision.Ambiguous -> RcsNativeTarget.Ambiguous
    NativeRcsTargetDecision.Unknown, NativeRcsTargetDecision.NotFoundComplete -> RcsNativeTarget.Unknown
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
