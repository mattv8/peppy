package dev.peppy.mobile.googlemessages

import java.util.concurrent.atomic.AtomicBoolean

/** Caller-thread-only coordinator for one trusted, ticket-bound native RCS operation. */
internal class RcsNativeSendCoordinator(
    private val go: RcsNativeGoOperation,
    private val coreLeaseFactory: RcsNativeCoreLeaseFactory,
    private val routeReader: RcsNativeRouteReader,
    private val clock: RcsNativeClock,
    private val canonicalizerRevision: Int,
) {
    private val executing = AtomicBoolean(false)

    fun execute(command: RcsNativeCommand): RcsNativeSendOutcome {
        if (!executing.compareAndSet(false, true)) return RcsNativeSendOutcome.REFUSED
        var ticketId: String? = null
        var lease: RcsNativeCoreLease? = null
        var preparation: RcsNativePreparation? = null
        var committed: RcsNativeCommittedHandle? = null
        var admitted = false
        var ticketOutstanding = false
        try {
            val route = routeReader.currentRoute() ?: return RcsNativeSendOutcome.REFUSED
            if (go.ownerGeneration() <= 0 || !go.isCurrent()) return RcsNativeSendOutcome.REFUSED
            val prepared = go.prepare(command, canonicalizerRevision, route).also {
                ticketId = it.ticketId
                ticketOutstanding = it.ticketId.isNotEmpty()
            }
            if (!prepared.valid()) return RcsNativeSendOutcome.REFUSED
            lease = coreLeaseFactory.acquire(prepared.ticketId) { go.revoke(prepared.ticketId) }
                ?: return RcsNativeSendOutcome.REFUSED

            val observedAtMs = clock.wallTimeMs()
            val tuple = lease.nextContextTuple(prepared.runIncarnation, prepared.runGeneration, prepared.streamEpoch, prepared.pairingEvidenceId)
            val revision = lease.nextMappingRevision(command.conversationId)
            val routeObservation = go.bindRoute(prepared.ticketId, tuple, revision, observedAtMs)
            if (lease.verifyMapping(routeObservation) != RcsNativeMappingVerification.GoogleConversationAmbiguous) return RcsNativeSendOutcome.REFUSED
            val target = lease.classifyTarget(command.conversationId, command.recipient, prepared.candidateSnapshotJson)
            if (target !is RcsNativeTarget.Found) return RcsNativeSendOutcome.REFUSED
            val observation = go.bindTarget(prepared.ticketId, target.googleConversationId, target.snapshotToken)
            if (lease.verifyMapping(observation) != RcsNativeMappingVerification.Mapped) return RcsNativeSendOutcome.REFUSED
            preparation = lease.prepare(command.commandId, clock.wallTimeMs())
            if (!go.bindFacts(prepared.ticketId, preparation.factsJson)) return RcsNativeSendOutcome.REFUSED
            val beginTimestamp = clock.wallTimeMs()
            val began = try {
                preparation.begin(observation, beginTimestamp)
            } catch (interrupted: InterruptedException) {
                Thread.currentThread().interrupt()
                return RcsNativeSendOutcome.OUTCOME_UNKNOWN
            } catch (_: Exception) {
                return RcsNativeSendOutcome.OUTCOME_UNKNOWN
            }
            val commitHandle = when (began) {
                RcsNativeBeginResult.KnownRefused -> return RcsNativeSendOutcome.REFUSED
                RcsNativeBeginResult.DurabilityUnknown -> return RcsNativeSendOutcome.OUTCOME_UNKNOWN
                is RcsNativeBeginResult.Committed -> began.handle
            }
            committed = commitHandle
            admitted = true
            val refreshed = routeReader.currentRoute()
            if (refreshed == null || !route.sameIdentity(refreshed) || !fresh(clock.wallTimeMs(), refreshed.observedAtMs) || !go.isCurrent()) {
                return recordRefusal(lease)
            }
            return when (go.commit(prepared.ticketId, commitHandle.exactCommitJson)) {
                RcsNativeCommitResult.Accepted -> if (
                    lease.recordTransportAcknowledged(commitHandle.tmpId, commitHandle.googleConversationId) &&
                    lease.recordSubmitted(commitHandle.tmpId, commitHandle.googleConversationId)
                ) RcsNativeSendOutcome.SUBMITTED else RcsNativeSendOutcome.OUTCOME_UNKNOWN
                RcsNativeCommitResult.Rejected -> recordRefusal(lease)
                RcsNativeCommitResult.OutcomeUnknown -> RcsNativeSendOutcome.OUTCOME_UNKNOWN
            }
        } catch (interrupted: InterruptedException) {
            Thread.currentThread().interrupt()
            return if (admitted) RcsNativeSendOutcome.OUTCOME_UNKNOWN else RcsNativeSendOutcome.REFUSED
        } catch (_: Exception) {
            return if (admitted) RcsNativeSendOutcome.OUTCOME_UNKNOWN else RcsNativeSendOutcome.REFUSED
        } finally {
            if (ticketOutstanding) ticketId?.let { runCatching { go.revoke(it) } }
            runCatching { committed?.close() }
            runCatching { preparation?.close() }
            runCatching { lease?.close() }
            executing.set(false)
        }
    }

    private fun recordRefusal(lease: RcsNativeCoreLease) =
        if (lease.recordPreEffectRefusal()) RcsNativeSendOutcome.REFUSED else RcsNativeSendOutcome.OUTCOME_UNKNOWN

    private fun RcsNativePrepared.valid() = ticketId.isNotEmpty() && candidateSnapshotJson.isNotEmpty() && runIncarnation.isNotEmpty() && pairingEvidenceId.isNotEmpty() && runGeneration > 0u && streamEpoch > 0u

    private fun RcsNativeRouteEvidence.sameIdentity(other: RcsNativeRouteEvidence) =
        routeId == other.routeId && subscriptionId == other.subscriptionId && ownNumberRaw == other.ownNumberRaw && ownNumberSource == other.ownNumberSource && identityRegion == other.identityRegion

    private fun fresh(nowMs: Long, observedAtMs: Long): Boolean =
        observedAtMs >= 0 && nowMs >= observedAtMs && observedAtMs <= Long.MAX_VALUE - MAX_ROUTE_AGE_MS && nowMs <= observedAtMs + MAX_ROUTE_AGE_MS

    private companion object { const val MAX_ROUTE_AGE_MS = 30_000L }
}
