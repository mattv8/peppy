package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RelayWakeStateTest {
    private val plainBox = object : SecretBox { override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString(); override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray() }
    @Test fun candidateIsDistinctFromActiveAndDurableBeforeRegistration() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val state = RelayWakeStateStore(context.getSharedPreferences("relay-state-test", 0), plainBox)
        val first = state.newCandidate("one"); assertTrue(state.saveCandidate(first))
        assertEquals(first.route.routeId, state.candidate()!!.route.routeId)
        assertTrue(state.saveActive(first.route, first.tokenFingerprint))
        val rotated = state.newCandidate("two")
        assertNotEquals(first.route.routeId, rotated.route.routeId)
    }

    @Test fun reconcileConfirmsSavedChallengeBeforeCreatingAnotherRegistration() {
        val store = RelayWakeMemoryStore()
        val identity = relayIdentity("token")
        val registrationId = "00000000-0000-4000-8000-000000000001"
        val pending = RelayWakePending(identity, "https://relay.example", "hosted", "token", registrationId, RelayRoute("route", "manage", "wake"), "a".repeat(64), 0)
        store.pending = pending
        val calls = mutableListOf<String>()
        val coordinator = RelayWakeCoordinator(store, object : RelayWakeTransport { override fun post(path: String, body: String): HttpResult {
            calls += path
            return HttpResult(201, "{\"route_id\":\"route\",\"manage_credential\":\"manage\",\"wake_credential\":\"wake\"}")
        } }, RelayRoutePublisher { _, _ -> })

        coordinator.reconcile(identity, "https://relay.example", "hosted", "device", "token", "bearer", false)

        assertEquals(listOf("/v1/registrations/$registrationId/confirm"), calls)
        assertEquals("route", store.active?.route?.routeId)
    }

    @Test fun legacyCommunityOffCanEnableAndAcceptWakesAgain() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(context, Configuration.Builder().setExecutor(SynchronousExecutor()).build())
        val prefs = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        prefs.edit().clear().putBoolean("relay-enabled.v1", false).commit()
        assertEquals(RelayWakePreference.PAUSED, RelayWakeRuntime.preference(context))

        RelayWakeRuntime.setEnabled(context, "https://community.example", true)

        assertTrue(RelayWakeRuntime.acceptsWakes(context))
    }

    @Test fun legacyRouteMigrationUsesPreviousOriginBeforeRelayChange() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-migrate-origin", 0)
        val route = RelayRoute("00000000-0000-4000-8000-000000000009", "manage", "wake")
        val legacy = FakeRouteStore(route)
        val state = RelayWakeStateStore(prefs, plainBox)

        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer"), "https://relay-a.example", legacy, state))

        assertEquals("https://relay-a.example", state.cleanups().single().relayOrigin)
        assertTrue(legacy.cleared)
    }

    @Test fun failedLegacyMigrationPreservesCredentialForPreviousOrigin() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-migrate-fail", 0)
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000010", "manage", "wake"))
        val brokenBox = object : SecretBox { override fun seal(purpose: String, plaintext: ByteArray): String = throw IllegalStateException(); override fun open(purpose: String, sealed: String) = null }

        assertTrue(!RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer"), "https://relay-a.example", legacy, RelayWakeStateStore(prefs, brokenBox)))

        assertTrue(!legacy.cleared)
        assertEquals("00000000-0000-4000-8000-000000000010", legacy.route()!!.routeId)
    }

    @Test fun startupMigrationUsesPersistedOriginWithoutAToggle() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-startup-migrate", 0)
        prefs.edit().clear().putString("relay-origin.v1", "https://relay-a.example").commit()
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000011", "manage", "wake"))
        val state = RelayWakeStateStore(prefs, plainBox)

        // initialize() passes the persisted origin on every normal startup, not only from setEnabled.
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer"), prefs.getString("relay-origin.v1", null), legacy, state))

        assertEquals("https://relay-a.example", state.cleanups().single().relayOrigin)
        assertTrue(legacy.cleared)
    }

    @Test fun pausedWorkRetiresCommittedRouteAndReportsCleanupHonestly() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-paused-work", 0)
        prefs.edit().clear().commit()
        val state = RelayWakeStateStore(prefs, plainBox)
        state.active = RelayWakeActive(relayIdentity("fp"), "https://relay.example", "hosted", "fp",
            RelayRoute("00000000-0000-4000-8000-000000000021", "manage", "wake"), "bearer", true)

        val queued = RelayWakeRuntime.retirePausedWork(state, cleanupFailed = false)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, queued.status)
        assertTrue(queued.retry)
        assertNull(state.active)
        assertEquals(1, state.cleanups().size)

        assertTrue(state.clearCleanup())
        val settled = RelayWakeRuntime.retirePausedWork(state, cleanupFailed = false)
        assertEquals(RelayWakeStatus.PAUSED, settled.status)
        assertFalse(settled.retry)

        // A failed earlier cleanup keeps the paused worker retrying even when the queue looks empty.
        val failed = RelayWakeRuntime.retirePausedWork(state, cleanupFailed = true)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, failed.status)
        assertTrue(failed.retry)
    }
}

private class FakeRouteStore(private var saved: RelayRoute?) : RelayRouteStore {
    var cleared = false
    override fun route() = saved
    override fun save(route: RelayRoute) { saved = route }
    override fun clear() { cleared = true; saved = null }
    override fun pendingRegistration() = null
    override fun savePendingRegistration(id: String) = Unit
    override fun clearPendingRegistration() = Unit
}

private class RelayWakeMemoryStore : RelayWakeCoordinatorStore {
    override var pending: RelayWakePending? = null
    override var active: RelayWakeActive? = null
    override fun fingerprint(value: String) = value
}

private fun relayIdentity(token: String) = RelayWakeIdentity(
    "https://account.example", "vault", "device", "fcm", "production", token,
)
