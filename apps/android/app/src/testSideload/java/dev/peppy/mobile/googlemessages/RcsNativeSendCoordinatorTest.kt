package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.SyncDiagnostics
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

@org.junit.runner.RunWith(org.robolectric.RobolectricTestRunner::class)
@org.robolectric.annotation.Config(sdk = [35])
class RcsNativeSendCoordinatorTest {
    @Test fun `durable refusal schedules its status once while early refusals remain quiet`() {
        var uploads = 0
        val rejected = Harness(commitResult = RcsNativeCommitResult.Rejected, onRefusalRecorded = { uploads++ })
        assertEquals(RcsNativeSendOutcome.REFUSED, rejected.execute())
        assertEquals(1, rejected.lease.refusalRecords)
        assertEquals(1, uploads)

        val early = Harness(target = RcsNativeTarget.Unknown, onRefusalRecorded = { uploads++ })
        assertEquals(RcsNativeSendOutcome.REFUSED, early.execute())
        assertEquals(0, early.lease.refusalRecords)
        assertEquals(1, uploads)

        val failedWrite = Harness(commitResult = RcsNativeCommitResult.Rejected, refusalRecorded = false, onRefusalRecorded = { uploads++ })
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, failedWrite.execute())
        assertEquals(1, uploads)
    }

    @Test fun `group commit refusal schedules status and scheduling failure cannot change recorded outcome`() {
        var uploads = 0
        val changed = Harness(groupDigest = "digest", commitResult = RcsNativeCommitResult.MembersChanged, onRefusalRecorded = { uploads++ })
        assertEquals(RcsNativeSendOutcome.MEMBERS_CHANGED, changed.execute())
        assertEquals(1, uploads)

        val failedWake = Harness(commitResult = RcsNativeCommitResult.Rejected, onRefusalRecorded = { error("scheduler unavailable") })
        assertEquals(RcsNativeSendOutcome.REFUSED, failedWake.execute())
        assertEquals(0, failedWake.lease.outcomeUnknownRecords)
    }

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

    @Test fun `carrier only target revokes the ticket without binding or beginning`() {
        val harness = Harness(target = RcsNativeTarget.CarrierOnly)

        assertEquals(RcsNativeSendOutcome.CARRIER_ONLY, harness.execute())
        assertEquals(0, harness.go.bindTargetCalls)
        assertEquals(0, harness.lease.beginCalls)
        assertEquals(0, harness.go.commitCalls)
        assertEquals(1, harness.go.revokes)
    }

    @Test fun `guard rejection does not classify core facts begin or commit`() {
        val harness = Harness(guardTarget = null)

        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals("", harness.lease.classifiedSnapshot)
        assertEquals(0, harness.lease.beginCalls)
        assertEquals(0, harness.go.commitCalls)
    }

    @Test fun `prebegin failure refuses but begin failure is durability unknown`() {
        val prebegin = Harness(prepareFailure = true)
        assertEquals(RcsNativeSendOutcome.REFUSED, prebegin.execute())
        assertEquals(1, prebegin.go.revokes)
        val begin = Harness(beginFailure = true)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, begin.execute())
        assertEquals(1, begin.go.revokes)
        assertEquals(0, begin.go.commitCalls)
        assertEquals(1, begin.lease.outcomeUnknownRecords)
    }

    @Test fun `post admission uncertainty records the exact attempt once without rewriting accepted`() {
        listOf(
            Harness(commitResult = RcsNativeCommitResult.OutcomeUnknown),
            Harness(commitFailure = true),
            Harness(beginFailure = true),
        ).forEach { harness ->
            assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, harness.execute())
            assertEquals(1, harness.lease.outcomeUnknownRecords)
        }

        val refused = Harness(routeVerification = RcsNativeMappingVerification.Refused)
        assertEquals(RcsNativeSendOutcome.REFUSED, refused.execute())
        assertEquals(0, refused.lease.outcomeUnknownRecords)

        val accepted = Harness()
        assertEquals(RcsNativeSendOutcome.SUBMITTED, accepted.execute())
        assertEquals(0, accepted.lease.outcomeUnknownRecords)
    }

    @Test fun `route change after final reread revokes before commit and unregisters`() {
        val routeChanges = FakeRouteChangeRevoker()
        val harness = Harness(routeChanges = routeChanges, fireRouteChangeAfterFinalRead = true)
        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals(2, harness.go.revokes)
        assertEquals(1, harness.go.commitCalls)
        assertEquals(0, harness.go.dispatches)
        assertEquals(1, harness.lease.refusalRecords)
        assertEquals(1, routeChanges.closes)
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

    @Test fun `before effect persistence failure is unknown without a go effect`() {
        val harness = Harness(beforeEffectRecorded = false)
        assertEquals(RcsNativeSendOutcome.OUTCOME_UNKNOWN, harness.execute())
        assertEquals(1, harness.lease.beforeEffectCalls)
        assertEquals(0, harness.go.commitCalls)
        assertEquals(1, harness.lease.outcomeUnknownRecords)
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

    @Test fun `a one to one target sends no digest and verifies through the mapping path only`() {
        val harness = Harness()
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
        assertNull(harness.go.lastDigest)
        assertEquals(listOf(ROUTE_OBSERVATION, FULL_OBSERVATION), harness.lease.verified)
        assertTrue(harness.lease.groupVerified.isEmpty())
    }

    @Test fun `a live group binds the proved revision, sends the digest, and re-proves instead of mapping`() {
        val harness = Harness(groupDigest = "digest")
        assertEquals(RcsNativeSendOutcome.SUBMITTED, harness.execute())
        assertEquals("digest", harness.go.lastDigest)
        assertEquals(PROVED_REVISION, harness.go.routeRevision)
        assertEquals(listOf(ROUTE_OBSERVATION), harness.lease.groupRouteVerified)
        assertEquals(listOf(FULL_OBSERVATION), harness.lease.groupVerified)
        assertTrue(harness.lease.verified.isEmpty())
        assertEquals(FULL_OBSERVATION, harness.lease.begun)
        assertEquals(1, harness.go.commitCalls)
    }

    @Test fun `a group whose route context is not current refuses before classification or any admission`() {
        val harness = Harness(groupDigest = "digest", groupRouteCurrent = false)
        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals("", harness.lease.classifiedSnapshot)
        assertTrue(harness.lease.groupVerified.isEmpty())
        assertEquals(0, harness.lease.beginCalls)
        assertEquals(0, harness.go.commitCalls)
    }

    @Test fun `group diagnostics record one fixed reason per send and nothing for one to one`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            Harness(groupDigest = "digest").execute()
            Harness(groupDigest = "digest", groupRouteCurrent = false).execute()
            Harness(groupDigest = "digest", groupVerification = RcsNativeGroupVerification.MembersChanged).execute()
            Harness(groupDigest = "digest", beginFailure = true).execute()
            Harness(groupDigest = "digest", target = RcsNativeTarget.Unknown).execute()
            Harness(groupDigest = "digest", groupVerification = RcsNativeGroupVerification.Refused).execute()
            Harness(groupDigest = "digest", commitResult = RcsNativeCommitResult.Rejected).execute()
            Harness().execute()
        }
        // submitted, route_not_current, members_changed, outcome_unknown, target_refused, proof_refused, commit_rejected, direct submitted
        assertEquals(listOf(68L, 72L, 69L, 70L, 73L, 74L, 76L, 79L), records.map { it.substringAfterLast("29=").toLong() })
    }

    @Test fun `one to one diagnostics use only fixed direct send codes`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            Harness().execute()
            Harness(target = RcsNativeTarget.CarrierOnly).execute()
            Harness(routeVerification = RcsNativeMappingVerification.Refused).execute()
            Harness(target = RcsNativeTarget.Unknown).execute()
            Harness(beginFailure = true).execute()
        }

        assertEquals(listOf(79L, 80L, 81L, 82L, 86L), records.map { it.substringAfterLast("29=").toLong() })
    }

    @Test fun `group members changed before any admission refuses with no commit and no ledger row`() {
        listOf(
            Harness(groupDigest = "digest", groupVerification = RcsNativeGroupVerification.MembersChanged),
            Harness(groupDigest = "digest", beginMembersChanged = true),
            Harness(groupDigest = "digest", prepareMembersChanged = true),
        ).forEach { harness ->
            assertEquals(RcsNativeSendOutcome.MEMBERS_CHANGED, harness.execute())
            assertEquals(0, harness.go.commitCalls)
            assertEquals(0, harness.lease.refusalRecords)
        }
    }

    @Test fun `media wrapper settles group changes detected during prepare or verification`() {
        listOf(
            Harness(groupDigest = "digest", prepareMembersChanged = true),
            Harness(groupDigest = "digest", groupVerification = RcsNativeGroupVerification.MembersChanged),
        ).forEach { harness ->
            val outcomes = mutableListOf<String>()
            val core = object : RcsMediaSendCore {
                override fun decrypt(attachmentId: String, destinationDir: String): String = error("reference already uploaded")
                override fun markUploading(attachment: RcsMediaAttachment) = error("reference already uploaded")
                override fun markReferenceReady(attachment: RcsMediaAttachment, mediaRef: String, uploadedAtMs: Long) = error("reference already uploaded")
                override fun recordOutcome(attachment: RcsMediaAttachment, outcome: String, atMs: Long) { outcomes += outcome }
            }
            val media = RcsMediaSendCoordinator(
                RcsMediaPrivateFiles(androidx.test.core.app.ApplicationProvider.getApplicationContext()),
                RcsMediaTransferQueue(java.util.concurrent.Executor(Runnable::run)), core,
                object : RcsMediaSendGo { override fun upload(path: java.io.File, attachment: RcsMediaAttachment): RcsMediaUploadResult = error("reference already uploaded") },
                { command, _ -> harness.coordinator.execute(command) }, { 100L },
            )
            media.submit(RcsMediaAttachment(COMMAND.commandId, COMMAND.conversationId, COMMAND.recipient, 0u, "attachment", "image/jpeg", "photo.jpg", 1u, uploadRequired = false))
            assertEquals(listOf("members_changed"), outcomes)
            assertEquals(0, harness.go.commitCalls)
        }
    }

    @Test fun `go members changed at commit is a definite refusal that records the membership flag`() {
        val harness = Harness(groupDigest = "digest", commitResult = RcsNativeCommitResult.MembersChanged)
        assertEquals(RcsNativeSendOutcome.MEMBERS_CHANGED, harness.execute())
        assertEquals(1, harness.lease.refusalRecords)
        assertEquals(listOf(true), harness.lease.refusalMembersChangedLog)
        assertEquals(0, harness.lease.outcomeUnknownRecords)
    }

    @Test fun `a group that is not live refuses before any ticket is bound to a route`() {
        val harness = Harness(groupDigest = "digest", proved = false)
        assertEquals(RcsNativeSendOutcome.REFUSED, harness.execute())
        assertEquals(0, harness.go.bound)
        assertEquals(0, harness.go.commitCalls)
    }

    @Test fun `group prepare-time members changed and lease refusals record their group codes, one to one records direct-send refusal`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            Harness(groupDigest = "digest", prepareMembersChanged = true).execute()
            Harness(groupDigest = "digest", factoryReturnsNull = true).execute()
            Harness(factoryReturnsNull = true).execute()
        }
        // members_changed (group, prepare), lease_refused (group), direct_send_refused (1:1 lease refusal).
        assertEquals(listOf(69L, 77L, 0L), records.map { it.substringAfterLast("29=").toLong() })
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
        private val routeChanges: RcsNativeRouteChangeRevoker = FakeRouteChangeRevoker(),
        private val fireRouteChangeAfterFinalRead: Boolean = false,
        private val groupDigest: String? = null,
        private val groupVerification: RcsNativeGroupVerification = RcsNativeGroupVerification.Proven,
        private val beginMembersChanged: Boolean = false,
        private val prepareMembersChanged: Boolean = false,
        private val proved: Boolean = true,
        private val groupRouteCurrent: Boolean = true,
        private val guardTarget: RcsNativeTarget? = target,
        private val beforeEffectRecorded: Boolean = true,
        onRefusalRecorded: () -> Unit = {},
        acquire: ((String, () -> Unit) -> RcsNativeCoreLease?)? = null,
        onPrepare: (() -> Unit)? = null,
    ) {
        val go = FakeGo(bindTargetFailure, bindFacts, commitFailure, commitResult, invalidPrepared, currentAfterBegin, onPrepare, prepareMembersChanged) { guardTarget }
        val lease = FakeLease(routeVerification, target, bindFacts, prepareFailure, beginFailure, refusalRecorded, acknowledged, groupVerification, beginMembersChanged, proved, groupRouteCurrent, beforeEffectRecorded)
        private var reads = 0
        private var clockCalls = 0
        val coordinator = RcsNativeSendCoordinator(
            go,
            object : RcsNativeCoreLeaseFactory {
                override fun acquire(ticketId: String, revoker: () -> Unit): RcsNativeCoreLease? =
                    if (factoryReturnsNull) null else acquire?.invoke(ticketId, revoker) ?: lease
                override fun groupMemberDigest(conversationId: String): String? = groupDigest
            },
            RcsNativeRouteReader {
                if (reads++ == 0) ROUTE else refreshedRoute.also {
                    if (fireRouteChangeAfterFinalRead) (routeChanges as FakeRouteChangeRevoker).fire()
                }
            },
            RcsNativeClock { if (++clockCalls == clockThrowsAt) throw IllegalStateException("clock") else nowMs },
            1,
            routeChanges,
            onRefusalRecorded,
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
        private val prepareMembersChanged: Boolean,
        private val classification: () -> RcsNativeTarget?,
    ) : RcsNativeGoOperation {
        var prepares = 0; var revokes = 0; var commitCalls = 0; var dispatches = 0; var bindTargetCalls = 0; var boundFacts = ""; var committedJson = ""
        var lastRevoke: RcsNativeRevokeResult = RcsNativeRevokeResult.None
        var lastDigest: String? = null; var routeRevision: ULong? = null; var bound = 0
        private var currentCalls = 0
        private var ticketRevoked = false
        override fun ownerGeneration() = 1L
        override fun isCurrent() = ++currentCalls == 1 || currentAfterBegin
        override fun prepare(command: RcsNativeCommand, canonicalizerRevision: Int, route: RcsNativeRouteEvidence, expectedMemberDigest: String?): RcsNativePrepared {
            prepares++; lastDigest = expectedMemberDigest; ticketRevoked = false; onPrepare?.invoke()
            if (prepareMembersChanged) throw RcsNativeMembersChanged()
            return RcsNativePrepared("ticket", if (invalidPrepared && prepares == 1) "" else CANDIDATE, "run", 1u, 1u, "pair")
        }
        override fun bindRoute(ticketId: String, tuple: RcsNativeContextTuple, mappingRevision: ULong, observedAtMs: Long): String {
            bound++
            routeRevision = mappingRevision
            return ROUTE_OBSERVATION
        }
        override fun classifyTarget(ticketId: String, classify: (String) -> RcsNativeTarget): RcsNativeTarget? =
            classification()?.let { expected -> classify(CANDIDATE).takeIf { expected == it } }
        override fun bindTarget(ticketId: String, googleConversationId: String, snapshotToken: String): String {
            bindTargetCalls++
            if (bindTargetFailure) throw IllegalStateException("bind")
            return FULL_OBSERVATION
        }
        override fun bindFacts(ticketId: String, factsJson: String) = bindFactsResult.also { boundFacts = factsJson }
        override fun commit(ticketId: String, exactCommitJson: String): RcsNativeCommitResult {
            commitCalls++; if (commitFailure) throw IllegalStateException("commit"); committedJson = exactCommitJson; if (!ticketRevoked) dispatches++; return if (ticketRevoked) RcsNativeCommitResult.Rejected else commitResult
        }
        override fun revoke(ticketId: String): RcsNativeRevokeResult { revokes++; ticketRevoked = true; return RcsNativeRevokeResult.Late.also { lastRevoke = it } }
    }

    private class FakeLease(
        private val routeVerification: RcsNativeMappingVerification = RcsNativeMappingVerification.GoogleConversationAmbiguous,
        private val target: RcsNativeTarget = RcsNativeTarget.Found("google", "token"),
        private val bindFactsResult: Boolean = true,
        private val prepareFailure: Boolean = false,
        private val beginFailure: Boolean = false,
        private val refusalRecorded: Boolean = true,
        private val acknowledged: Boolean = true,
        private val groupVerification: RcsNativeGroupVerification = RcsNativeGroupVerification.Proven,
        private val beginMembersChanged: Boolean = false,
        private val proved: Boolean = true,
        private val groupRouteCurrent: Boolean = true,
        private val beforeEffectRecorded: Boolean = true,
    ) : RcsNativeCoreLease {
        var classifiedSnapshot = ""; val verified = mutableListOf<String>(); val groupRouteVerified = mutableListOf<String>(); val groupVerified = mutableListOf<String>(); var begun = ""; var coreCalls = 0; var closes = 0; var preparationCloses = 0; var committedCloses = 0; var refusalRecords = 0; var outcomeUnknownRecords = 0; var beginCalls = 0; var acknowledgements = 0; var beforeEffectCalls = 0
        val refusalMembersChangedLog = mutableListOf<Boolean>()
        override fun provedGroupMemberRevision(conversationId: String): ULong? = if (proved) PROVED_REVISION else null
        override fun verifyGroupRoute(exactObservationJson: String): Boolean { groupRouteVerified += exactObservationJson; coreCalls++; return groupRouteCurrent }
        override fun classifyGroupTarget(conversationId: String, exactSnapshotJson: String): RcsNativeTarget = target.also { classifiedSnapshot = exactSnapshotJson; coreCalls++ }
        override fun verifyGroupObservation(exactObservationJson: String): RcsNativeGroupVerification { groupVerified += exactObservationJson; coreCalls++; return groupVerification }
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
                if (beginMembersChanged) RcsNativeBeginResult.MembersChanged
                else RcsNativeBeginResult.Committed(object : RcsNativeCommittedHandle(COMMIT, "core-tmp", "google") { override fun close() { committedCloses++ } })
            }) { override fun close() { preparationCloses++ } }
        }
        override fun recordTransportAcknowledged(tmpId: String, googleConversationId: String) = acknowledged.also { acknowledgements++ }
        override fun recordSubmitted(tmpId: String, googleConversationId: String) = true
        override fun recordPreEffectRefusal(membersChanged: Boolean) = refusalRecorded.also {
            refusalRecords++
            refusalMembersChangedLog += membersChanged
        }
        override fun recordOutcomeUnknown() = true.also { outcomeUnknownRecords++ }
        override fun beforeEffect() = beforeEffectRecorded.also { beforeEffectCalls++ }
        override fun close() { closes++ }
    }

    private class FakeRouteChangeRevoker : RcsNativeRouteChangeRevoker {
        private var revoke: (() -> Unit)? = null
        var closes = 0
        override fun register(boundRoute: RcsNativeRouteEvidence, revokeTicket: () -> Unit): AutoCloseable {
            revoke = revokeTicket
            return AutoCloseable { closes++ }
        }
        fun fire() = revoke?.invoke()
    }

    private companion object {
        val PROVED_REVISION = 7uL
        val COMMAND = RcsNativeCommand("command", "conversation", "+15550000")
        val ROUTE = RcsNativeRouteEvidence("route", 1, "+15551111", RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber, "US", 99)
        const val CANDIDATE = " { \\\"candidate\\\" : \\\"x\\\\\\\"y\\\" } "
        const val ROUTE_OBSERVATION = " { \\\"route\\\" : [ 1 ] } "
        const val FULL_OBSERVATION = " { \\\"full\\\" : \\\"x\\\\\\\"y\\\" } "
        const val FACTS = " { \\\"facts\\\" : \\\"x\\\\\\\"y\\\" } "
        const val COMMIT = " { \\\"tmp_id\\\" : \\\"core-tmp\\\", \\\"message_body\\\" : \\\"core body\\\" } "
    }
}
