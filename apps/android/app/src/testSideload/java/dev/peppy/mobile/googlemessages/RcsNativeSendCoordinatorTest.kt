package dev.peppy.mobile.googlemessages

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class RcsNativeSendCoordinatorTest {
    @Test fun `forwards opaque bytes in the required order before a single commit`() {
        val harness = Harness()
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
        assertEquals(CANDIDATE, harness.lease.classifiedSnapshot)
        assertEquals(listOf(ROUTE_OBSERVATION, FULL_OBSERVATION), harness.lease.verified)
        assertEquals(FULL_OBSERVATION, harness.lease.begun)
        assertEquals(FACTS, harness.go.boundFacts)
        assertEquals(COMMIT, harness.go.committedJson)
        assertEquals(1, harness.go.commitCalls)
    }

    @Test fun `ticket before lease refusal and held retirement revoke only its ticket`() {
        val held = Harness(factoryReturnsNull = true)
        assertEquals(RcsNativeSendOutcome.REFUSED, held.execute())
        assertEquals(1, held.go.revokes)
        assertEquals(0, held.lease.coreCalls)

        var revoker: (() -> Unit)? = null
        val retired = Harness(acquire = { _, revoke -> revoker = revoke; retiredLease() })
        revoker = null
        assertEquals(RcsNativeSendOutcome.SUBMITTED, retired.execute())
        revoker?.invoke()
        assertEquals(1, retired.go.commitCalls)
        assertEquals(RcsNativeRevokeResult.Late, retired.go.lastRevoke)

        val fresh = Harness()
        assertEquals(RcsNativeSendOutcome.SUBMITTED, fresh.execute())
        assertEquals(1, fresh.go.commitCalls)
    }

    @Test fun `refusal branches never commit`() {
        val cases = listOf(
            Harness(routeVerification = RcsNativeMappingVerification.Refused),
            Harness(target = RcsNativeTarget.Unknown),
            Harness(target = RcsNativeTarget.Ambiguous),
            Harness(bindFacts = false),
            Harness(bindTargetFailure = true),
        )
        cases.forEach { harness ->
            assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
            assertEquals(0, harness.go.commitCalls)
            assertEquals(1, harness.lease.closes)
        }
    }

    @Test fun `prebegin failure refuses but begin failure is durability unknown`() {
        val prebegin = Harness(prepareFailure = true)
        assertEquals(RcsNativeSendOutcome.REFUSED, prebegin.execute())
        assertEquals(1, prebegin.go.revokes)
        val begin = Harness(beginFailure = true)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, begin.execute())
        assertEquals(1, begin.go.revokes)
        assertEquals(0, begin.go.commitCalls)
    }

    @Test fun `post begin drift records preeffect refusal and recorder failure is unknown`() {
        val drift = Harness(refreshedRoute = ROUTE.copy(ownNumberRaw = "+15559999"))
        assertEquals(RcsNativeSendOutcome.REFUSED, drift.execute())
        assertEquals(1, drift.lease.refusalRecords)
        assertEquals(0, drift.go.commitCalls)
        val persistenceFailure = Harness(currentAfterBegin = false, refusalRecorded = false)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, persistenceFailure.execute())
        assertEquals(1, persistenceFailure.lease.refusalRecords)
    }

    @Test fun `accepted persistence failures and go commit exception are unknown without retry`() {
        val ackFailure = Harness(acknowledged = false)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, ackFailure.execute())
        assertEquals(1, ackFailure.go.commitCalls)
        val commitFailure = Harness(commitFailure = true)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, commitFailure.execute())
        assertEquals(1, commitFailure.go.commitCalls)
        assertEquals(1, commitFailure.go.revokes)
    }

    @Test fun `owned invalid prepared ticket is revoked even when its metadata is refused`() {
        val harness = Harness(invalidPrepared = true)
        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals(1, harness.go.revokes)
        assertEquals(0, harness.lease.coreCalls)
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
    }

    @Test fun `rejected unconsumed ticket is recorded then revoked and accepted late revoke preserves submission`() {
        val rejected = Harness(commitResult = RcsNativeCommitResult.Rejected)
        assertEquals(RcsNativeSendOutcome.REFUSED, rejected.execute())
        assertEquals(1, rejected.lease.refusalRecords)
        assertEquals(1, rejected.go.revokes)
        assertEquals(RcsNativeSendOutcome.SUBMITTED, Harness().execute())
        val accepted = Harness()
        assertEquals(RcsNativeSendOutcome.SUBMITTED, accepted.execute())
        assertEquals(1, accepted.go.revokes)
        assertEquals(1, accepted.lease.acknowledgements)
    }

    @Test fun `route freshness is inclusive through thirty seconds and rejects future overflow`() {
        listOf(29_999L to RcsNativeSendOutcome.SUBMITTED, 30_000L to RcsNativeSendOutcome.SUBMITTED, 30_001L to RcsNativeSendOutcome.REFUSED).forEach { (age, expected) ->
            val now = 100_000L
            val harness = Harness(nowMs = now, refreshedRoute = ROUTE.copy(observedAtMs = now - age))
            assertEquals(expected, harness.execute())
        }
        assertEquals(RcsNativeSendOutcome.REFUSED, Harness(nowMs = 100_000, refreshedRoute = ROUTE.copy(observedAtMs = 100_001)).execute())
        assertEquals(RcsNativeSendOutcome.REFUSED, Harness(nowMs = Long.MAX_VALUE, refreshedRoute = ROUTE.copy(observedAtMs = Long.MAX_VALUE)).execute())
    }

    @Test fun `reentrant execution is refused without another prepare and resources close once`() {
        lateinit var coordinator: RcsNativeSendCoordinator
        val harness = Harness(onPrepare = { assertEquals(RcsNativeSendOutcome.REFUSED, coordinator.execute(COMMAND)) })
        coordinator = harness.coordinator
        assertEquals(RcsNativeSendOutcome.SUBMITTED, coordinator.execute(COMMAND))
        assertEquals(1, harness.go.prepares)
        assertEquals(1, harness.lease.closes)
        assertEquals(1, harness.lease.preparationCloses)
        assertEquals(1, harness.lease.committedCloses)
    }

    @Test fun `clock failure before begin refuses without starting core admission`() {
        val harness = Harness(clockThrowsAt = 3)
        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals(0, harness.lease.beginCalls)
        assertEquals(1, harness.go.revokes)
        assertEquals(1, harness.lease.closes)
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
    }

    @Test fun `post begin clock failure revokes the outstanding ticket once and releases the gate`() {
        val harness = Harness(clockThrowsAt = 4)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, harness.execute())
        assertEquals(1, harness.lease.beginCalls)
        assertEquals(0, harness.go.commitCalls)
        assertEquals(1, harness.go.revokes)
        assertEquals(1, harness.lease.closes)
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
    }

    private fun retiredLease() = FakeLease()

    private class Harness(
        private val routeVerification: RcsNativeMappingVerification = RcsNativeMappingVerification.GoogleConversationAmbiguous,
        private val target: RcsNativeTarget = RcsNativeTarget.Found("google", "token"),
        private val bindFacts: Boolean = true,
        private val bindTargetFailure: Boolean = false,
        private val prepareFailure: Boolean = false,
        private val beginFailure: Boolean = false,
        private val refreshedRoute: RcsNativeRouteEvidence = ROUTE,
        private val currentAfterBegin: Boolean = true,
        private val refusalRecorded: Boolean = true,
        private val acknowledged: Boolean = true,
        private val commitFailure: Boolean = false,
        private val commitResult: RcsNativeCommitResult = RcsNativeCommitResult.Accepted,
        private val invalidPrepared: Boolean = false,
        private val nowMs: Long = 100L,
        private val clockThrowsAt: Int = 0,
        private val factoryReturnsNull: Boolean = false,
        acquire: ((String, () -> Unit) -> RcsNativeCoreLease?)? = null,
        onPrepare: (() -> Unit)? = null,
    ) {
        val go = FakeGo(bindTargetFailure, bindFacts, commitFailure, commitResult, invalidPrepared, currentAfterBegin, onPrepare)
        val lease = FakeLease(routeVerification, target, bindFacts, prepareFailure, beginFailure, refusalRecorded, acknowledged)
        private var reads = 0
        private var clockCalls = 0
        val coordinator = RcsNativeSendCoordinator(
            go,
            RcsNativeCoreLeaseFactory { ticket, revoke -> if (factoryReturnsNull) null else acquire?.invoke(ticket, revoke) ?: lease },
            RcsNativeRouteReader { if (reads++ == 0) ROUTE else refreshedRoute },
            RcsNativeClock { if (++clockCalls == clockThrowsAt) throw IllegalStateException("clock") else nowMs },
            1,
        )
        fun execute() = coordinator.execute(COMMAND)
    }

    private class FakeGo(
        private val bindTargetFailure: Boolean,
        private val bindFactsResult: Boolean,
        private val commitFailure: Boolean,
        private val commitResult: RcsNativeCommitResult,
        private val invalidPrepared: Boolean,
        private val currentAfterBegin: Boolean,
        private val onPrepare: (() -> Unit)?,
    ) : RcsNativeGoOperation {
        var prepares = 0; var revokes = 0; var commitCalls = 0; var boundFacts = ""; var committedJson = ""
        var lastRevoke: RcsNativeRevokeResult = RcsNativeRevokeResult.None
        private var currentCalls = 0
        override fun ownerGeneration() = 1L
        override fun isCurrent() = ++currentCalls == 1 || currentAfterBegin
        override fun prepare(command: RcsNativeCommand, canonicalizerRevision: Int, route: RcsNativeRouteEvidence): RcsNativePrepared {
            prepares++; onPrepare?.invoke(); return RcsNativePrepared("ticket", if (invalidPrepared && prepares == 1) "" else CANDIDATE, "run", 1u, 1u, "pair")
        }
        override fun bindRoute(ticketId: String, tuple: RcsNativeContextTuple, mappingRevision: ULong, observedAtMs: Long) = ROUTE_OBSERVATION
        override fun bindTarget(ticketId: String, googleConversationId: String, snapshotToken: String): String {
            if (bindTargetFailure) throw IllegalStateException("bind")
            return FULL_OBSERVATION
        }
        override fun bindFacts(ticketId: String, factsJson: String) = bindFactsResult.also { boundFacts = factsJson }
        override fun commit(ticketId: String, exactCommitJson: String): RcsNativeCommitResult {
            commitCalls++; if (commitFailure) throw IllegalStateException("commit"); committedJson = exactCommitJson; return commitResult
        }
        override fun revoke(ticketId: String): RcsNativeRevokeResult { revokes++; return RcsNativeRevokeResult.Late.also { lastRevoke = it } }
    }

    private class FakeLease(
        private val routeVerification: RcsNativeMappingVerification = RcsNativeMappingVerification.GoogleConversationAmbiguous,
        private val target: RcsNativeTarget = RcsNativeTarget.Found("google", "token"),
        private val bindFactsResult: Boolean = true,
        private val prepareFailure: Boolean = false,
        private val beginFailure: Boolean = false,
        private val refusalRecorded: Boolean = true,
        private val acknowledged: Boolean = true,
    ) : RcsNativeCoreLease {
        var classifiedSnapshot = ""; val verified = mutableListOf<String>(); var begun = ""; var coreCalls = 0; var closes = 0; var preparationCloses = 0; var committedCloses = 0; var refusalRecords = 0; var beginCalls = 0; var acknowledgements = 0
        override fun nextContextTuple(runIncarnation: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String) = RcsNativeContextTuple(1u, 1u, 1u).also { coreCalls++ }
        override fun nextMappingRevision(conversationId: String) = 1uL.also { coreCalls++ }
        override fun verifyMapping(exactObservationJson: String): RcsNativeMappingVerification { verified += exactObservationJson; coreCalls++; return if (exactObservationJson == ROUTE_OBSERVATION) routeVerification else RcsNativeMappingVerification.Mapped }
        override fun classifyTarget(conversationId: String, recipient: String, exactSnapshotJson: String) = target.also { classifiedSnapshot = exactSnapshotJson; coreCalls++ }
        override fun prepare(commandId: String, nowMs: Long): RcsNativePreparation {
            coreCalls++; if (prepareFailure) throw IllegalStateException("prepare")
            return object : RcsNativePreparation(FACTS, { observation, _ ->
                beginCalls++
                begun = observation
                if (beginFailure) throw IllegalStateException("begin")
                RcsNativeBeginResult.Committed(object : RcsNativeCommittedHandle(COMMIT, "core-tmp", "google") { override fun close() { committedCloses++ } })
            }) { override fun close() { preparationCloses++ } }
        }
        override fun recordTransportAcknowledged(tmpId: String, googleConversationId: String) = acknowledged.also { acknowledgements++ }
        override fun recordSubmitted(tmpId: String, googleConversationId: String) = true
        override fun recordPreEffectRefusal() = refusalRecorded.also { refusalRecords++ }
        override fun close() { closes++ }
    }

    private companion object {
        val COMMAND = RcsNativeCommand("command", "conversation", "+15550000")
        val ROUTE = RcsNativeRouteEvidence("route", 1, "+15551111", RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber, "US", 99)
        const val CANDIDATE = " { \\\"candidate\\\" : \\\"x\\\\\\\"y\\\" } "
        const val ROUTE_OBSERVATION = " { \\\"route\\\" : [ 1 ] } "
        const val FULL_OBSERVATION = " { \\\"full\\\" : \\\"x\\\\\\\"y\\\" } "
        const val FACTS = " { \\\"facts\\\" : \\\"x\\\\\\\"y\\\" } "
        const val COMMIT = " { \\\"tmp_id\\\" : \\\"core-tmp\\\", \\\"message_body\\\" : \\\"core body\\\" } "
    }
}
