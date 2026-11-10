package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import java.util.concurrent.atomic.AtomicBoolean

/** Caller-thread-only coordinator for one trusted, ticket-bound native RCS operation. */
internal class RcsNativeSendCoordinator(
    private val go: RcsNativeGoOperation,
    private val coreLeaseFactory: RcsNativeCoreLeaseFactory,
    private val routeReader: RcsNativeRouteReader,
    private val clock: RcsNativeClock,
    private val canonicalizerRevision: Int,
    private val routeChangeRevoker: RcsNativeRouteChangeRevoker,
) {
    private val executing = AtomicBoolean(false)

    fun execute(command: RcsNativeCommand): RcsNativeSendOutcome {
        if (!executing.compareAndSet(false, true)) return RcsNativeSendOutcome.REFUSED
        val trace = GroupSendTrace()
        try {
            val outcome = attempt(command, trace)
            if (trace.group) recordGroupSend(outcome, trace.reason)
            return outcome
        } finally {
            executing.set(false)
        }
    }

    private fun attempt(command: RcsNativeCommand, trace: GroupSendTrace): RcsNativeSendOutcome {
        var ticketId: String? = null
        var lease: RcsNativeCoreLease? = null
        var preparation: RcsNativePreparation? = null
        var committed: RcsNativeCommittedHandle? = null
        var admitted = false
        var ticketOutstanding = false
        var routeChangeRegistration: AutoCloseable? = null
        try {
            val route = routeReader.currentRoute() ?: return RcsNativeSendOutcome.REFUSED
            if (go.ownerGeneration() <= 0 || !go.isCurrent()) return RcsNativeSendOutcome.REFUSED
            val observedAtMs = clock.wallTimeMs()
            // A Live group passes its stored member digest to Go as advisory input; a 1:1 target passes null.
            val groupDigest = coreLeaseFactory.groupMemberDigest(command.conversationId)
            // Group identity is known before prepare, so prepare-time and lease refusals carry group codes.
            val group = groupDigest != null
            trace.group = group
            val prepared = go.prepare(command, canonicalizerRevision, route, groupDigest).also {
                ticketId = it.ticketId
                ticketOutstanding = it.ticketId.isNotEmpty()
            }
            if (!prepared.valid()) return RcsNativeSendOutcome.REFUSED
            routeChangeRegistration = routeChangeRevoker.register(route) { go.revoke(prepared.ticketId) }
            lease = coreLeaseFactory.acquire(prepared.ticketId) { go.revoke(prepared.ticketId) }
                ?: return trace.refused("lease_refused")

            val tuple = lease.nextContextTuple(prepared.runIncarnation, prepared.runGeneration, prepared.streamEpoch, prepared.pairingEvidenceId)
            // A group binds the proved revision; its proof time is refreshed by the re-proof below, not by a new revision.
            val revision = if (group) lease.provedGroupMemberRevision(command.conversationId) ?: return RcsNativeSendOutcome.REFUSED
                else lease.nextMappingRevision(command.conversationId)
            val routeObservation = go.bindRoute(prepared.ticketId, tuple, revision, observedAtMs)
            // Both targets advance the same link context; only the 1:1 route is verified as a mapping.
            val routeCurrent = if (group) lease.verifyGroupRoute(routeObservation)
                else lease.verifyMapping(routeObservation) == RcsNativeMappingVerification.GoogleConversationAmbiguous
            if (!routeCurrent) return trace.refused("route_not_current")
            val target = if (group) lease.classifyGroupTarget(command.conversationId, prepared.candidateSnapshotJson)
                else lease.classifyTarget(command.conversationId, command.recipient, prepared.candidateSnapshotJson)
            if (target !is RcsNativeTarget.Found) return trace.refused("target_refused")
            val observation = go.bindTarget(prepared.ticketId, target.googleConversationId, target.snapshotToken)
            if (group) {
                // Core pauses the group on a roster change and admitted nothing, so there is no ledger row to refuse.
                when (lease.verifyGroupObservation(observation)) {
                    RcsNativeGroupVerification.Proven -> Unit
                    RcsNativeGroupVerification.MembersChanged -> return RcsNativeSendOutcome.MEMBERS_CHANGED
                    RcsNativeGroupVerification.Refused -> return trace.refused("proof_refused")
                }
            } else if (lease.verifyMapping(observation) != RcsNativeMappingVerification.Mapped) {
                return RcsNativeSendOutcome.REFUSED
            }
            preparation = lease.prepare(command.commandId, clock.wallTimeMs())
            if (!go.bindFacts(prepared.ticketId, preparation.factsJson)) return RcsNativeSendOutcome.REFUSED
            val beginTimestamp = clock.wallTimeMs()
            val began = try {
                preparation.begin(observation, beginTimestamp)
            } catch (interrupted: InterruptedException) {
                Thread.currentThread().interrupt()
                return recordOutcomeUnknown(lease)
            } catch (_: Exception) {
                return recordOutcomeUnknown(lease)
            }
            val commitHandle = when (began) {
                RcsNativeBeginResult.KnownRefused -> return trace.refused("begin_refused")
                RcsNativeBeginResult.MembersChanged -> return RcsNativeSendOutcome.MEMBERS_CHANGED
                RcsNativeBeginResult.DurabilityUnknown -> return recordOutcomeUnknown(lease)
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
                ) RcsNativeSendOutcome.SUBMITTED else recordOutcomeUnknown(lease)
                RcsNativeCommitResult.Rejected -> {
                    trace.reason = "commit_rejected"
                    recordRefusal(lease)
                }
                RcsNativeCommitResult.MembersChanged -> recordMembersChanged(lease)
                RcsNativeCommitResult.OutcomeUnknown -> recordOutcomeUnknown(lease)
            }
        } catch (_: RcsNativeMembersChanged) {
            // Prepare refused a group before any ticket was bound or any Google call was made.
            return RcsNativeSendOutcome.MEMBERS_CHANGED
        } catch (interrupted: InterruptedException) {
            Thread.currentThread().interrupt()
            return if (admitted) recordOutcomeUnknown(lease) else RcsNativeSendOutcome.REFUSED
        } catch (_: Exception) {
            return if (admitted) recordOutcomeUnknown(lease) else RcsNativeSendOutcome.REFUSED
        } finally {
            runCatching { routeChangeRegistration?.close() }
            if (ticketOutstanding) ticketId?.let { runCatching { go.revoke(it) } }
            runCatching { committed?.close() }
            runCatching { preparation?.close() }
            runCatching { lease?.close() }
        }
    }

    /** Records one bounded group outcome with a fixed reason code. No ids, members, or message content. */
    private fun recordGroupSend(outcome: RcsNativeSendOutcome, reason: String?) {
        val code = when (outcome) {
            RcsNativeSendOutcome.SUBMITTED -> "submitted"
            RcsNativeSendOutcome.MEMBERS_CHANGED -> "members_changed"
            RcsNativeSendOutcome.OUTCOME_UNKNOWN -> "outcome_unknown"
            RcsNativeSendOutcome.REFUSED -> reason ?: "refused"
        }
        recordGoogleMessagesLinkEvent(GROUP_SEND_KIND, code, 0L)
    }

    private fun recordRefusal(lease: RcsNativeCoreLease) =
        if (lease.recordPreEffectRefusal()) RcsNativeSendOutcome.REFUSED else recordOutcomeUnknown(lease)

    /** Go's `members_changed` at commit is a definite not-sent: core fails the attempt and pauses the group. */
    private fun recordMembersChanged(lease: RcsNativeCoreLease) =
        if (lease.recordPreEffectRefusal(membersChanged = true)) RcsNativeSendOutcome.MEMBERS_CHANGED else recordOutcomeUnknown(lease)

    private fun recordOutcomeUnknown(lease: RcsNativeCoreLease?): RcsNativeSendOutcome {
        lease?.recordOutcomeUnknown()
        return RcsNativeSendOutcome.OUTCOME_UNKNOWN
    }

    private fun RcsNativePrepared.valid() = ticketId.isNotEmpty() && candidateSnapshotJson.isNotEmpty() && runIncarnation.isNotEmpty() && pairingEvidenceId.isNotEmpty() && runGeneration > 0u && streamEpoch > 0u

    private fun RcsNativeRouteEvidence.sameIdentity(other: RcsNativeRouteEvidence) =
        routeId == other.routeId && subscriptionId == other.subscriptionId && ownNumberRaw == other.ownNumberRaw && ownNumberSource == other.ownNumberSource && identityRegion == other.identityRegion

    private fun fresh(nowMs: Long, observedAtMs: Long): Boolean =
        observedAtMs >= 0 && nowMs >= observedAtMs && observedAtMs <= Long.MAX_VALUE - MAX_ROUTE_AGE_MS && nowMs <= observedAtMs + MAX_ROUTE_AGE_MS

    private companion object {
        const val MAX_ROUTE_AGE_MS = 30_000L
        const val GROUP_SEND_KIND = "group_send"
    }
}

/** One send's diagnostics: whether it targets a Live group, and the fixed reason of a group refusal. */
private class GroupSendTrace {
    var group = false
    var reason: String? = null

    fun refused(code: String): RcsNativeSendOutcome {
        reason = code
        return RcsNativeSendOutcome.REFUSED
    }
}
