package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.NativeGateway
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.NativeRcsTargetDecision
import uniffi.peppy_mobile_bindings.gatewayNextRcsContextTuple
import uniffi.peppy_mobile_bindings.gatewayNextRcsMappingRevision
import uniffi.peppy_mobile_bindings.gatewayPrepareNativeRcsSend
import uniffi.peppy_mobile_bindings.gatewayProvedGroupMemberDigest
import uniffi.peppy_mobile_bindings.gatewayRecordRcsOutcomeUnknown
import uniffi.peppy_mobile_bindings.gatewayVerifyRcsGroupRoute

class RcsNativeCoreAdapterTest {
    @Test fun `forwards opaque input and exact unsigned tuple values`() {
        val port = FakePort()
        val lease = lease(port)
        val tuple = lease.nextContextTuple("run", ULong.MAX_VALUE, 9u, "pairing")
        assertEquals(RcsNativeContextTuple(ULong.MAX_VALUE, 9u, 11u), tuple)
        lease.verifyMapping(" { opaque } ")
        lease.classifyTarget("conversation", "recipient", " [ snapshot ] ")
        assertEquals(" { opaque } ", port.observation)
        assertEquals(" [ snapshot ] ", port.snapshot)
        assertEquals(listOf(101L, 102L), port.nowValues)
    }

    @Test fun `not found complete maps conservatively to unknown`() {
        assertEquals(RcsNativeMappingVerification.Mapped, nativeRcsMappingVerification(NativeRcsMappingVerification.MAPPED))
        assertEquals(RcsNativeMappingVerification.GoogleConversationAmbiguous, nativeRcsMappingVerification(NativeRcsMappingVerification.GOOGLE_CONVERSATION_AMBIGUOUS))
        assertEquals(RcsNativeMappingVerification.Refused, nativeRcsMappingVerification(NativeRcsMappingVerification.REFUSED))
        assertEquals(RcsNativeTarget.Found("google", "token"), nativeRcsTarget(NativeRcsTargetDecision.Found("google", "token")))
        assertEquals(RcsNativeTarget.Ambiguous, nativeRcsTarget(NativeRcsTargetDecision.Ambiguous))
        assertEquals(RcsNativeTarget.Unknown, nativeRcsTarget(NativeRcsTargetDecision.Unknown))
        assertEquals(RcsNativeTarget.Unsupported, nativeRcsTarget(NativeRcsTargetDecision.Unsupported))
        assertEquals(RcsNativeTarget.Unknown, nativeRcsTarget(NativeRcsTargetDecision.NotFoundComplete))
    }

    @Test fun `group members changed at begin is a definite refusal, not an unknown outcome`() {
        val preparation = FakePreparation(beginFailure = MobileBindingsException.GroupMembersChanged())
        val wrapped = lease(FakePort(preparation = preparation)).prepare("command", 0)
        assertEquals(RcsNativeBeginResult.MembersChanged, wrapped.begin("observation", 9))
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, wrapped.begin("again", 10))
        assertEquals(1, preparation.beginCalls)
    }

    @Test fun `group route step passes the exact observation and the lease clock to the port`() {
        val port = FakePort().apply { groupRouteCurrent = false }
        val lease = lease(port)
        assertFalse(lease.verifyGroupRoute(" { route } "))
        assertEquals(" { route } ", port.groupRoute)
        assertEquals(listOf(101L), port.nowValues)
    }

    @Test fun `group re-proof and proved revision pass through the lease with the lease clock`() {
        val port = FakePort().apply { groupVerification = RcsNativeGroupVerification.MembersChanged }
        val lease = lease(port)
        assertEquals(42uL, lease.provedGroupMemberRevision("conversation"))
        assertEquals(RcsNativeGroupVerification.MembersChanged, lease.verifyGroupObservation(" { group } "))
        assertEquals(" { group } ", port.groupObservation)
        assertEquals(listOf(101L), port.nowValues)
    }

    @Test fun `refusal carries the membership flag to the port`() {
        val port = FakePort()
        val lease = lease(port)
        lease.prepare("command", 0).begin("observation", 0) as RcsNativeBeginResult.Committed
        assertTrue(lease.recordPreEffectRefusal(membersChanged = true))
        assertEquals(listOf(true), port.refusalMembersChanged)
    }

    @Test fun `begin failures are unknown after the one native call`() {
        val preparation = FakePreparation(beginFailure = IllegalStateException())
        val lease = lease(FakePort(preparation = preparation))
        val wrapped = lease.prepare("command", 0)
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, wrapped.begin("observation", 9))
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, wrapped.begin("again", 10))
        assertEquals(1, preparation.beginCalls)
    }

    @Test fun `committed IDs fence outcome writes and refusal uses committed IDs`() {
        val port = FakePort()
        val lease = lease(port)
        val begin = lease.prepare("command", 0).begin("observation", 0) as RcsNativeBeginResult.Committed
        assertFalse(lease.recordTransportAcknowledged("other", begin.handle.googleConversationId))
        assertFalse(lease.recordSubmitted(begin.handle.tmpId, "other"))
        assertTrue(lease.recordPreEffectRefusal())
        assertEquals(listOf("refusal:command:tmp:google"), port.effects)
    }

    @Test fun `facts failure closes native preparation once and close fences calls`() {
        val preparation = FakePreparation(factsFailure = IllegalStateException())
        val lease = lease(FakePort(preparation = preparation))
        runCatching { lease.prepare("command", 0) }
        assertEquals(1, preparation.closeCalls)
        lease.close()
        lease.close()
        assertEquals(1, released)
        assertTrue(runCatching { lease.nextMappingRevision("c") }.isFailure)
    }

    @Test fun `closed preparation never invokes native begin`() {
        val preparation = FakePreparation()
        val wrapped = lease(FakePort(preparation = preparation)).prepare("command", 0)
        wrapped.close()
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, wrapped.begin("observation", 0))
        assertEquals(0, preparation.beginCalls)
    }

    @Test fun `mismatched command closes a throwing handle once and never records an outcome`() {
        val committed = FakeCommitted(commandId = "different", closeFailure = IllegalStateException())
        val port = FakePort(preparation = FakePreparation(committed = committed))
        val lease = lease(port)
        val result = lease.prepare("command", 0).begin("observation", 0)
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, result)
        assertEquals(1, committed.closeCalls)
        assertFalse(lease.recordTransportAcknowledged("tmp", "google"))
        assertFalse(lease.recordSubmitted("tmp", "google"))
        assertFalse(lease.recordPreEffectRefusal())
        assertTrue(port.effects.isEmpty())
    }

    @Test fun `exact facts and commit are forwarded and both native handles close once`() {
        val committed = FakeCommitted(commitJson = " { exact commit } ")
        val preparation = FakePreparation(facts = " [ exact facts ] ", committed = committed)
        val wrapped = lease(FakePort(preparation = preparation)).prepare("command", 0)
        assertEquals(" [ exact facts ] ", wrapped.factsJson)
        val result = wrapped.begin("observation", 0) as RcsNativeBeginResult.Committed
        assertEquals(" { exact commit } ", result.handle.exactCommitJson)
        result.handle.close()
        result.handle.close()
        wrapped.close()
        wrapped.close()
        assertEquals(1, committed.closeCalls)
        assertEquals(1, preparation.closeCalls)
    }

    @Test fun `committed getter failure closes its native handle once`() {
        val committed = FakeCommitted(commitFailure = IllegalStateException())
        val result = lease(FakePort(preparation = FakePreparation(committed = committed))).prepare("command", 0).begin("observation", 0)
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, result)
        assertEquals(1, committed.closeCalls)
    }

    @Test fun `successful acknowledgement and submission use admitted command`() {
        val port = FakePort()
        val lease = lease(port)
        val result = lease.prepare("command", 0).begin("observation", 0) as RcsNativeBeginResult.Committed
        assertTrue(lease.recordTransportAcknowledged(result.handle.tmpId, result.handle.googleConversationId))
        assertTrue(lease.recordSubmitted(result.handle.tmpId, result.handle.googleConversationId))
        assertEquals(listOf("ack:command", "submitted:command"), port.effects)
    }

    @Test fun `translated interruption restores the interrupted flag`() {
        val preparation = FakePreparation(beginFailure = InterruptedException())
        val result = lease(FakePort(preparation = preparation)).prepare("command", 0).begin("observation", 0)
        assertEquals(RcsNativeBeginResult.DurabilityUnknown, result)
        assertTrue(Thread.interrupted())
    }

    private fun lease(port: FakePort): NativeRcsCoreLease = NativeRcsCoreLease(port, RcsNativeClock { ++now }, { released++ })

    private class FakePort(
        private val target: RcsNativeTarget = RcsNativeTarget.Found("google", "token"),
        private val preparation: FakePreparation = FakePreparation(),
    ) : NativeRcsCorePort {
        var observation = ""
        var snapshot = ""
        val nowValues = mutableListOf<Long>()
        val effects = mutableListOf<String>()
        override fun nextContextTuple(runtimeId: String, runGeneration: ULong, streamEpoch: ULong, pairingEvidenceId: String) = RcsNativeContextTuple(runGeneration, streamEpoch, 11u)
        override fun nextMappingRevision(conversationId: String) = ULong.MAX_VALUE
        override fun verifyMapping(observationJson: String, nowMs: Long): RcsNativeMappingVerification { observation = observationJson; nowValues += nowMs; return RcsNativeMappingVerification.Mapped }
        override fun classifyTarget(conversationId: String, recipient: String, snapshotJson: String, nowMs: Long): RcsNativeTarget { snapshot = snapshotJson; nowValues += nowMs; return target }
        override fun prepare(commandId: String, nowMs: Long) = preparation
        override fun recordTransportAcknowledged(commandId: String, ackAtMs: Long) { effects += "ack:$commandId" }
        override fun recordSubmitted(commandId: String) { effects += "submitted:$commandId" }
        override fun recordPreEffectRefusal(commandId: String, tmpId: String, googleConversationId: String, membersChanged: Boolean) {
            effects += "refusal:$commandId:$tmpId:$googleConversationId"
            refusalMembersChanged += membersChanged
        }
        val refusalMembersChanged = mutableListOf<Boolean>()
        var groupVerification = RcsNativeGroupVerification.Proven
        var groupObservation = ""
        var groupRouteCurrent = true
        var groupRoute = ""
        override fun verifyGroupRoute(observationJson: String, nowMs: Long): Boolean {
            groupRoute = observationJson
            nowValues += nowMs
            return groupRouteCurrent
        }
        override fun provedGroupMemberRevision(conversationId: String): ULong? = 42uL
        override fun verifyGroupObservation(observationJson: String, nowMs: Long): RcsNativeGroupVerification {
            groupObservation = observationJson
            nowValues += nowMs
            return groupVerification
        }
        override fun recordOutcomeUnknown(commandId: String, tmpId: String, googleConversationId: String) { effects += "unknown:$commandId:$tmpId:$googleConversationId" }
    }

    private class FakePreparation(
        private val facts: String = "facts",
        private val factsFailure: Throwable? = null,
        private val beginFailure: Throwable? = null,
        private val committed: FakeCommitted = FakeCommitted(),
    ) : NativeRcsPreparationPort {
        var beginCalls = 0
        var closeCalls = 0
        override fun factsJson(): String = factsFailure?.let { throw it } ?: facts
        override fun begin(observationJson: String, nowMs: Long): NativeRcsCommittedPort {
            beginCalls++
            beginFailure?.let { throw it }
            return committed
        }
        override fun close() { closeCalls++ }
    }

    private class FakeCommitted(
        private val commandId: String = "command",
        private val commitJson: String = "commit",
        private val commitFailure: Throwable? = null,
        private val closeFailure: Throwable? = null,
    ) : NativeRcsCommittedPort {
        var closeCalls = 0
        override fun commandId() = commandId
        override fun commitJson(): String = commitFailure?.let { throw it } ?: commitJson
        override fun tmpId() = "tmp"
        override fun googleConversationId() = "google"
        override fun close() {
            closeCalls++
            closeFailure?.let { throw it }
        }
    }

    private var now = 100L
    private var released = 0
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsNativeCoreAdapterGatewaySmokeTest : GatewayTestBase() {
    @Test fun `factory keeps its captured NativeGateway owner after retirement`() {
        enrollAndUnlock()
        val nativeLease = checkNotNull(NativeGateway.acquireRcsSendLease(context, "ticket-native") {})
        assertEquals(
            1uL,
            gatewayNextRcsMappingRevision(nativeLease.client, "00000000-0000-4000-8000-000000000001"),
        )
        assertEquals(
            1uL,
            gatewayNextRcsContextTuple(nativeLease.client, "native-smoke-runtime", 1u, 1u, "native-smoke-pairing").contextRevision,
        )
        assertTrue(runCatching { gatewayPrepareNativeRcsSend(nativeLease.client, "foreign-command", 1L) }.isFailure)
        // Through the real library: no digest for a conversation that is not a Live group, and an
        // unparseable route observation is refused rather than advancing the context.
        assertEquals(null, gatewayProvedGroupMemberDigest(nativeLease.client, "00000000-0000-4000-8000-000000000001"))
        assertTrue(runCatching { gatewayVerifyRcsGroupRoute(nativeLease.client, "not json", 1L) }.isFailure)
        assertTrue(runCatching { gatewayRecordRcsOutcomeUnknown(nativeLease.client, "foreign-command", "foreign-tmp", "foreign-google") }.isFailure)
        val adapter = RcsNativeCoreAdapter(context, RcsNativeClock { 1L })
        var revocations = 0
        val retiredLease = checkNotNull(adapter.acquire("ticket-retired") { revocations++ })
        assertEquals(
            RcsNativeContextTuple(1u, 1u, 1u),
            retiredLease.nextContextTuple("runtime-a", 3u, 5u, "pairing-a"),
        )

        NativeGateway.closeForTest()
        assertEquals(1, revocations)
        checkNotNull(NativeGateway.open(context))

        assertTrue(runCatching {
            retiredLease.nextContextTuple("runtime-stale", 7u, 11u, "pairing-stale")
        }.isFailure)
        assertTrue(runCatching {
            gatewayNextRcsMappingRevision(nativeLease.client, "00000000-0000-4000-8000-000000000002")
        }.isFailure)

        val currentLease = checkNotNull(adapter.acquire("ticket-current") {})
        assertEquals(
            RcsNativeContextTuple(1u, 1u, 1u),
            currentLease.nextContextTuple("runtime-b", 13u, 17u, "pairing-b"),
        )
        retiredLease.close()
        currentLease.close()
        nativeLease.close()
    }
}
