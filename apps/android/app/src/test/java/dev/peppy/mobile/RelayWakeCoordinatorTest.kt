package dev.peppy.mobile

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.IOException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RelayWakeCoordinatorTest {
    @Test fun oneRegisterThenChallengePerformsOneConfirm() {
        val fake = FakeTransport()
        val fixture = fixture(fake)
        assertEquals(RelayWakeReconcile.PENDING, fixture.reconcile())
        fixture.coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.REGISTERED, fixture.reconcile())
        assertEquals(1, fake.registers)
        assertEquals(1, fake.confirms.size)
    }

    @Test fun earlyChallengeWaitsForCommittedRegistrationIdThenConfirms() {
        val fake = FakeTransport()
        lateinit var coordinator: RelayWakeCoordinator
        fake.onRegister = { coordinator.saveChallenge(CHALLENGE) }
        val fixture = fixture(fake)
        coordinator = fixture.coordinator
        assertEquals(RelayWakeReconcile.PENDING, fixture.reconcile())
        // The callback's early slot is attached only after the id is durable.
        coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.REGISTERED, fixture.reconcile())
        assertEquals(1, fake.registers)
        assertEquals(1, fake.confirms.size)
    }

    @Test fun staleChallengeClearsPendingBeforeRetryingRegistration() {
        val fake = FakeTransport().also { it.confirmCode = 400 }
        val fixture = fixture(fake)
        fixture.reconcile(); fixture.coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        assertNull(fixture.store.pending)
        fixture.reconcile()
        assertEquals(2, fake.registers)
    }

    @Test fun lostConfirmResponseReplaysSameCandidateAndProof() {
        val fake = FakeTransport().also { it.failFirstConfirm = true }
        val fixture = fixture(fake)
        fixture.reconcile(); fixture.coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        assertEquals(RelayWakeReconcile.REGISTERED, fixture.reconcile())
        assertEquals(fake.confirms[0], fake.confirms[1])
        assertEquals(1, fake.registers)
    }

    @Test fun failedPublishRetriesOnlySavedRoute() {
        val fake = FakeTransport()
        var publishCalls = 0
        val fixture = fixture(fake, publisher = RelayRoutePublisher { _, _ -> if (publishCalls++ == 0) throw IOException() })
        fixture.reconcile(); fixture.coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        val route = fixture.store.active!!.route.routeId
        assertEquals(RelayWakeReconcile.REGISTERED, fixture.reconcile())
        assertEquals(route, fixture.store.active!!.route.routeId)
        assertEquals(1, fake.registers)
    }

    @Test fun pauseDuringPublishDoesNotResurrectQueuedRoute() {
        val fake = FakeTransport()
        val store = MemoryStore()
        val retired = mutableListOf<RelayWakeActive>()
        lateinit var coordinator: RelayWakeCoordinator
        coordinator = RelayWakeCoordinator(
            store,
            fake,
            RelayRoutePublisher { _, _ -> },
            retire = { retired += it; true },
            publishActive = { store.active = null },
        )
        coordinator.reconcile(identity("token"), RELAY, "hosted", "device", "device-token", "bearer", true)
        coordinator.saveChallenge(CHALLENGE)

        assertEquals(RelayWakeReconcile.RETRY, coordinator.reconcile(identity("token"), RELAY, "hosted", "device", "device-token", "bearer", true))
        assertNull(store.active)
        assertEquals(1, retired.size)
        val oldRoute = retired.single().route.routeId
        assertEquals(RelayWakeReconcile.PENDING, coordinator.reconcile(identity("token"), RELAY, "hosted", "device", "new-token", "bearer", true))
        assertNotEquals(oldRoute, store.pending!!.candidate.routeId)
    }

    @Test fun rotationQueuesOldRouteWithItsCapturedBearer() {
        val fake = FakeTransport()
        val store = MemoryStore()
        val old = RelayWakeActive(identity("old"), RELAY, "hosted", "old-token", RelayRoute(ROUTE, MANAGE, WAKE), "old-bearer", true)
        store.active = old
        var retired: RelayWakeActive? = null
        val coordinator = coordinator(store, fake, retire = { retired = it; true })
        coordinator.reconcile(identity("new"), RELAY, "hosted", "device", "new-token", "new-bearer", true)
        assertEquals(ROUTE, retired?.route?.routeId)
        assertEquals("old-bearer", retired?.bearerToken)
    }

    @Test fun registerIOExceptionRepostsSavedCandidateOnNextReconcile() {
        val fake = FakeTransport().also { it.failFirstRegister = true }
        val fixture = fixture(fake)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        val candidate = fixture.store.pending!!.candidate.routeId
        assertEquals(RelayWakeReconcile.PENDING, fixture.reconcile())
        assertEquals(2, fake.registers)
        assertEquals(candidate, fixture.store.pending!!.candidate.routeId)
    }

    @Test fun register429RepostsSavedCandidateOnNextReconcile() {
        val fake = FakeTransport().also { it.registerCode = 429 }
        val fixture = fixture(fake)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        fake.registerCode = 201
        assertEquals(RelayWakeReconcile.PENDING, fixture.reconcile())
        assertEquals(2, fake.registers)
    }

    @Test fun expiredRegisteredChallengeStartsFreshRegistration() {
        val fake = FakeTransport()
        val store = MemoryStore()
        store.pending = RelayWakePending(identity("token"), RELAY, "hosted", "device-token", REGISTRATION, RelayRoute(ROUTE, MANAGE, WAKE), null, 0)
        val coordinator = coordinator(store, fake, now = { RelayWakeCoordinator.CHALLENGE_TTL_MILLIS + 1 })
        assertEquals(RelayWakeReconcile.PENDING, coordinator.reconcile(identity("token"), RELAY, "hosted", "device", "device-token", "bearer", true))
        assertEquals(1, fake.registers)
        assertNotEquals(ROUTE, store.pending!!.candidate.routeId)
    }

    @Test fun confirm429KeepsProofForRetry() {
        val fake = FakeTransport().also { it.confirmCode = 429 }
        val fixture = fixture(fake)
        fixture.reconcile(); fixture.coordinator.saveChallenge(CHALLENGE)
        assertEquals(RelayWakeReconcile.RETRY, fixture.reconcile())
        assertEquals(CHALLENGE, fixture.store.pending!!.challenge)
        fake.confirmCode = 201
        assertEquals(RelayWakeReconcile.REGISTERED, fixture.reconcile())
    }

    @Test fun sameTokenWithNewIdentityReenrolls() {
        val fake = FakeTransport()
        val store = MemoryStore()
        store.active = RelayWakeActive(identity("old"), RELAY, "hosted", "same-token", RelayRoute(ROUTE, MANAGE, WAKE), "old", true)
        val coordinator = coordinator(store, fake)
        coordinator.reconcile(identity("new"), RELAY, "hosted", "device", "same-token", "new", true)
        assertEquals(1, fake.registers)
    }

    @Test fun legacyConfirmOmitsCandidatesAndHostedIncludesThem() {
        val legacy = FakeTransport(); val legacyFixture = fixture(legacy, candidates = false)
        legacyFixture.reconcile(); legacyFixture.coordinator.saveChallenge(CHALLENGE); legacyFixture.reconcile()
        assertFalse(JSONObject(legacy.confirms.single()).has("route_id"))
        val hosted = FakeTransport(); val hostedFixture = fixture(hosted, candidates = true)
        hostedFixture.reconcile(); hostedFixture.coordinator.saveChallenge(CHALLENGE); hostedFixture.reconcile()
        assertTrue(JSONObject(hosted.confirms.single()).has("route_id"))
    }

    @Test fun coldRestartWithMatchingBindingMakesNoRelayCalls() {
        val fake = FakeTransport(); val store = MemoryStore()
        store.active = RelayWakeActive(identity("token"), RELAY, "hosted", "device-token", RelayRoute(ROUTE, MANAGE, WAKE), "bearer", true)
        assertEquals(RelayWakeReconcile.REGISTERED, coordinator(store, fake).reconcile(identity("token"), RELAY, "hosted", "device", "device-token", "bearer", true))
        assertEquals(0, fake.registers)
    }

    @Test fun hostedCandidateGateReadsOnlyRelayCapabilitiesEndpoint() {
        val fake = FakeTransport().also { it.capabilities = false }
        assertFalse(relaySupportsCandidates(fake)!!)
        assertEquals(listOf("/v1/capabilities"), fake.gets)
        fake.capabilities = true
        assertTrue(relaySupportsCandidates(fake)!!)
    }

    private fun fixture(fake: FakeTransport, candidates: Boolean = true, publisher: RelayRoutePublisher = RelayRoutePublisher { _, _ -> }) = Fixture(MemoryStore(), fake, candidates, publisher).also { it.coordinator = coordinator(it.store, fake, it.publisher) }
    private fun coordinator(store: MemoryStore, fake: FakeTransport, publisher: RelayRoutePublisher = RelayRoutePublisher { _, _ -> }, retire: (RelayWakeActive) -> Boolean = { true }, now: () -> Long = System::currentTimeMillis) = RelayWakeCoordinator(store, fake, publisher, retire = retire, nowMillis = now)

    private class Fixture(val store: MemoryStore, val fake: FakeTransport, val candidates: Boolean, val publisher: RelayRoutePublisher) {
        lateinit var coordinator: RelayWakeCoordinator
        fun reconcile() = coordinator.reconcile(identity("token"), RELAY, "hosted", "device", "device-token", "bearer", candidates)
    }
    private class MemoryStore : RelayWakeCoordinatorStore { override var pending: RelayWakePending? = null; override var active: RelayWakeActive? = null; override fun fingerprint(value: String) = value }
    private class FakeTransport : RelayWakeTransport {
        var registers = 0; val confirms = mutableListOf<String>(); val gets = mutableListOf<String>(); var capabilities = true; var registerCode = 201; var failFirstRegister = false; var confirmCode = 201; var failFirstConfirm = false; var onRegister: (() -> Unit)? = null
        override fun get(path: String): HttpResult { gets += path; return HttpResult(200, "{\"confirm_candidates\":$capabilities}") }
        override fun post(path: String, body: String): HttpResult = when {
            path == "/v1/registrations" -> { registers++; if (failFirstRegister && registers == 1) throw IOException(); onRegister?.invoke(); HttpResult(registerCode, "{\"registration_id\":\"$REGISTRATION\"}") }
            else -> {
                confirms += body
                if (failFirstConfirm && confirms.size == 1) throw IOException()
                val request = JSONObject(body)
                val route = request.optString("route_id", ROUTE)
                val manage = request.optString("manage_credential", MANAGE)
                val wake = request.optString("wake_credential", WAKE)
                HttpResult(confirmCode, "{\"route_id\":\"$route\",\"manage_credential\":\"$manage\",\"wake_credential\":\"$wake\"}")
            }
        }
    }
    private companion object {
        const val RELAY = "https://relay.example"; const val ROUTE = "00000000-0000-4000-8000-000000000002"; const val ROUTE_TWO = "00000000-0000-4000-8000-000000000003"; const val REGISTRATION = "00000000-0000-4000-8000-000000000001"
        val MANAGE = "a".repeat(64); val WAKE = "b".repeat(64); val CHALLENGE = "c".repeat(64)
        fun identity(token: String) = RelayWakeIdentity("https://account.example", "vault", "device", "fcm", "production", token)
    }
}
