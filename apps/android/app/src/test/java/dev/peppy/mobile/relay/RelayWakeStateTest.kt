package dev.peppy.mobile.relay

import android.content.Context
import android.content.SharedPreferences
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import dev.peppy.mobile.RelayRoute
import dev.peppy.mobile.RelayRoutePublisher
import dev.peppy.mobile.RelayRouteStore
import dev.peppy.mobile.RelayWakePreference
import dev.peppy.mobile.RelayWakeRuntime
import dev.peppy.mobile.RelayWakeStatus
import dev.peppy.mobile.RelayWakeTransport
import dev.peppy.mobile.gateway.GatewayAccountCredential
import dev.peppy.mobile.gateway.HttpResult
import dev.peppy.mobile.security.SecretBox
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.Assert.*
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

        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L), "https://relay-a.example", legacy, state))

        assertEquals("https://relay-a.example", state.cleanups().single().relayOrigin)
        assertTrue(legacy.cleared)
    }

    @Test fun failedLegacyMigrationPreservesCredentialForPreviousOrigin() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-migrate-fail", 0)
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000010", "manage", "wake"))
        val brokenBox = object : SecretBox { override fun seal(purpose: String, plaintext: ByteArray): String = throw IllegalStateException(); override fun open(purpose: String, sealed: String) = null }

        assertTrue(!RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L), "https://relay-a.example", legacy, RelayWakeStateStore(prefs, brokenBox)))

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
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L), prefs.getString("relay-origin.v1", null), legacy, state))

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

    @Test fun falseCommitBlocksFreshWrapperUntilExactPendingWriteIsDurable() {
        val preferences = MemoryMutatingPreferences()
        val identity = relayIdentity("fingerprint")
        val candidate = RelayRoute("00000000-0000-4000-8000-000000000041", "manage", "wake")
        val pending = RelayWakePending(identity, "https://relay.example", "hosted", "fingerprint", null, candidate, null, 1L)
        preferences.failCommits = true

        assertFalse(RelayWakeStateStore(preferences, plainBox).savePending(pending))
        assertNull(RelayWakeStateStore(preferences.reopen(), plainBox).pending)

        var networkCalls = 0
        fun coordinator() = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, plainBox),
            object : RelayWakeTransport {
                override fun post(path: String, body: String): HttpResult {
                    networkCalls++
                    return HttpResult(201, "{\"registration_id\":\"00000000-0000-4000-8000-000000000042\"}")
                }
            },
            RelayRoutePublisher { _, _ -> networkCalls++ },
        )
        assertEquals(RelayWakeReconcile.RETRY, coordinator().reconcile(identity, "https://relay.example", "hosted", "device", "fingerprint", "bearer", true))
        assertEquals(0, networkCalls)

        preferences.failCommits = false
        assertEquals(RelayWakeReconcile.PENDING, coordinator().reconcile(identity, "https://relay.example", "hosted", "device", "fingerprint", "bearer", true))
        assertEquals(1, networkCalls)
        val reopened = RelayWakeStateStore(preferences.reopen(), plainBox).pending
        assertEquals(candidate.routeId, reopened?.candidate?.routeId)
        assertEquals("00000000-0000-4000-8000-000000000042", reopened?.registrationId)
    }

    @Test fun falsePromotionCommitCannotPublishUntilAtomicTransferRetriesDurably() {
        val preferences = MemoryMutatingPreferences()
        val identity = relayIdentity("fingerprint")
        val route = RelayRoute("00000000-0000-4000-8000-000000000051", "manage", "wake")
        val pending = RelayWakePending(
            identity, "https://relay.example", "hosted", "fingerprint",
            "00000000-0000-4000-8000-000000000052", route, "a".repeat(64), 1L,
        )
        assertTrue(RelayWakeStateStore(preferences, plainBox).savePending(pending))
        preferences.failCommits = true
        var confirms = 0
        var publishes = 0

        fun coordinator() = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, plainBox),
            object : RelayWakeTransport {
                override fun post(path: String, body: String): HttpResult {
                    confirms++
                    return HttpResult(201, "{\"route_id\":\"${route.routeId}\",\"manage_credential\":\"manage\",\"wake_credential\":\"wake\"}")
                }
            },
            RelayRoutePublisher { _, _ -> publishes++ },
        )

        assertEquals(RelayWakeReconcile.RETRY, coordinator().reconcile(identity, "https://relay.example", "hosted", "device", "fingerprint", "bearer", true))
        assertEquals(1, confirms)
        assertEquals(0, publishes)
        assertNull(RelayWakeStateStore(preferences.reopen(), plainBox).active)
        assertNotNull(RelayWakeStateStore(preferences.reopen(), plainBox).pending)

        assertEquals(RelayWakeReconcile.RETRY, coordinator().reconcile(identity, "https://relay.example", "hosted", "device", "fingerprint", "bearer", true))
        assertEquals(1, confirms)
        assertEquals(0, publishes)

        preferences.failCommits = false
        assertEquals(RelayWakeReconcile.REGISTERED, coordinator().reconcile(identity, "https://relay.example", "hosted", "device", "fingerprint", "bearer", true))
        assertEquals(1, confirms)
        assertEquals(1, publishes)
        val reopened = RelayWakeStateStore(preferences.reopen(), plainBox)
        assertNull(reopened.pending)
        assertTrue(reopened.active!!.published)
    }

    @Test fun failedChallengeWritesRemainRetryableAndAbsentAfterRealRestart() {
        val preferences = MemoryMutatingPreferences()
        val identity = relayIdentity("fingerprint")
        val pending = RelayWakePending(
            identity, "https://relay.example", "hosted", "fingerprint",
            "00000000-0000-4000-8000-000000000071",
            RelayRoute("00000000-0000-4000-8000-000000000072", "manage", "wake"), null, 1L,
        )
        assertTrue(RelayWakeStateStore(preferences, plainBox).savePending(pending))
        preferences.failCommits = true
        val challenge = "b".repeat(64)
        val first = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, plainBox),
            object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
            RelayRoutePublisher { _, _ -> },
        )
        assertFalse(first.saveChallenge(challenge))
        assertNull(RelayWakeStateStore(preferences.reopen(), plainBox).pending?.challenge)

        val fresh = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, plainBox),
            object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
            RelayRoutePublisher { _, _ -> },
        )
        assertFalse(fresh.saveChallenge(challenge))
        preferences.failCommits = false
        assertTrue(fresh.saveChallenge(challenge))
        assertEquals(challenge, RelayWakeStateStore(preferences.reopen(), plainBox).pending?.challenge)

        preferences.failCommits = true
        assertFalse(RelayWakeStateStore(preferences, plainBox).saveEarlyChallenge(challenge))
        assertNull(RelayWakeStateStore(preferences.reopen(), plainBox).earlyChallenge())
        preferences.failCommits = false
        val recovered = RelayWakeStateStore(preferences, plainBox)
        assertTrue(recovered.recoverAcknowledgedState())
        assertEquals(challenge, RelayWakeStateStore(preferences.reopen(), plainBox).earlyChallenge())
    }

    @Test fun failedRetirementRemovalRemainsCleanupPendingAcrossWrappers() {
        val preferences = MemoryMutatingPreferences()
        val first = RelayWakeStateStore(preferences, plainBox)
        val active = RelayWakeActive(
            relayIdentity("fingerprint"), "https://relay.example", "hosted", "fingerprint",
            RelayRoute("00000000-0000-4000-8000-000000000061", "manage", "wake"), "bearer", true,
        )
        assertTrue(first.saveActive(active))
        preferences.successfulCommitsBeforeFailure = 1

        val failed = RelayWakeRuntime.retirePausedWork(first, cleanupFailed = false)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, failed.status)
        assertTrue(failed.retry)
        val fresh = RelayWakeRuntime.retirePausedWork(RelayWakeStateStore(preferences, plainBox), cleanupFailed = false)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, fresh.status)
        assertTrue(fresh.retry)

        preferences.successfulCommitsBeforeFailure = null
        val recovered = RelayWakeRuntime.retirePausedWork(RelayWakeStateStore(preferences, plainBox), cleanupFailed = false)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, recovered.status)
        assertTrue(recovered.retry)
        val reopened = RelayWakeStateStore(preferences.reopen(), plainBox)
        assertNull(reopened.active)
        assertEquals(active.route.routeId, reopened.cleanups().single().route.routeId)
    }

    @Test fun unreadableCleanupBlocksCompletionAndRecoversWithoutChangingCiphertext() {
        val preferences = MemoryMutatingPreferences()
        val box = TemporarilyUnreadableBox()
        val cleanup = RelayWakeCleanup(
            relayIdentity("fingerprint"),
            RelayRoute("00000000-0000-4000-8000-000000000081", "manage", "wake"),
            "bearer",
            "https://relay.example",
        )
        val writer = RelayWakeStateStore(preferences, box)
        assertTrue(writer.saveCleanup(cleanup))
        val reopenedPreferences = preferences.reopen()
        val sealed = reopenedPreferences.getString("hosted-relay-cleanup.v2.${cleanup.route.routeId}", null)
        box.readable = false
        val unreadable = RelayWakeStateStore(reopenedPreferences, box)

        assertFalse(unreadable.recoverAcknowledgedState())
        assertThrows(RelayWakeStateUnavailableException::class.java) { unreadable.cleanups() }
        val paused = RelayWakeRuntime.retirePausedWork(unreadable, cleanupFailed = false)
        assertEquals(RelayWakeStatus.CLEANUP_PENDING, paused.status)
        assertTrue(paused.retry)
        assertEquals(sealed, reopenedPreferences.getString("hosted-relay-cleanup.v2.${cleanup.route.routeId}", null))

        box.readable = true
        val recovered = RelayWakeStateStore(reopenedPreferences, box)
        assertTrue(recovered.recoverAcknowledgedState())
        assertEquals(cleanup.route.routeId, recovered.cleanups().single().route.routeId)
        assertEquals(sealed, reopenedPreferences.getString("hosted-relay-cleanup.v2.${cleanup.route.routeId}", null))
    }

    @Test fun malformedPendingBlocksReplacementAndPreservesStoredBytes() {
        val preferences = MemoryMutatingPreferences()
        val key = "hosted-relay-pending.v2"
        assertTrue(preferences.edit().putString(key, "malformed-protected-state").commit())
        var effects = 0
        val coordinator = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, plainBox),
            object : RelayWakeTransport {
                override fun post(path: String, body: String): HttpResult { effects++; return HttpResult(500, null) }
            },
            RelayRoutePublisher { _, _ -> effects++ },
        )

        assertEquals(
            RelayWakeReconcile.RETRY,
            coordinator.reconcile(relayIdentity("fingerprint"), "https://relay.example", "hosted", "device", "fingerprint", "bearer", true),
        )
        assertEquals(0, effects)
        assertEquals("malformed-protected-state", preferences.getString(key, null))
    }

    @Test fun archiveQueuedAcrossWrapperCannotBeOverwrittenByPublishAcknowledgement() {
        val preferences = MemoryMutatingPreferences()
        val box = PublishAckLatchBox()
        val state = RelayWakeStateStore(preferences, box)
        val active = RelayWakeActive(
            relayIdentity("fingerprint"), "https://relay.example", "hosted", "fingerprint",
            RelayRoute("00000000-0000-4000-8000-000000000091", "manage", "wake"), "bearer", false,
        )
        assertTrue(state.saveActive(active))
        var result: RelayWakeReconcile? = null
        val reconcileThread = thread {
            result = RelayWakeCoordinator(
                RelayWakeStateStore(preferences, box),
                object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
                RelayRoutePublisher { _, _ -> },
                publishActive = {},
            ).reconcile(active.identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, active.bearerToken, true)
        }
        assertTrue(box.ackSealEntered.await(5, TimeUnit.SECONDS))
        val archiveStarted = CountDownLatch(1)
        val archiveFinished = CountDownLatch(1)
        val archiveThread = thread {
            archiveStarted.countDown()
            val archive = RelayWakeStateStore(preferences, box)
            assertTrue(archive.saveCleanup(RelayWakeCleanup(active.identity, active.route, active.bearerToken, active.relayOrigin)))
            assertTrue(archive.saveActive(null))
            archiveFinished.countDown()
        }
        assertTrue(archiveStarted.await(5, TimeUnit.SECONDS))
        assertTrue(box.archiveSealEntered.await(5, TimeUnit.SECONDS))
        assertFalse(archiveFinished.await(200, TimeUnit.MILLISECONDS))
        box.releaseAckSeal.countDown()
        reconcileThread.join(5_000)
        archiveThread.join(5_000)

        assertFalse(reconcileThread.isAlive)
        assertFalse(archiveThread.isAlive)
        assertNull(RelayWakeStateStore(preferences.reopen(), box).active)
        assertEquals(active.route.routeId, RelayWakeStateStore(preferences.reopen(), box).cleanups().single().route.routeId)
        assertEquals(RelayWakeReconcile.REGISTERED, result)
    }

    @Test fun publicationAckSealFailureKeepsCurrentRouteAndFreshWrapperCanAcknowledge() {
        val preferences = MemoryMutatingPreferences()
        val box = OneShotPublicationSealFailureBox()
        val active = RelayWakeActive(
            relayIdentity("fingerprint"), "https://relay.example", "hosted", "fingerprint",
            RelayRoute("00000000-0000-4000-8000-000000000095", "manage", "wake"), "bearer", false,
        )
        val initial = RelayWakeStateStore(preferences, box)
        assertTrue(initial.saveActive(active))
        var publishes = 0
        var retires = 0
        fun coordinator(store: RelayWakeStateStore) = RelayWakeCoordinator(
            store,
            object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
            RelayRoutePublisher { _, _ -> },
            retire = { retired ->
                retires++
                store.saveCleanup(RelayWakeCleanup(retired.identity, retired.route, retired.bearerToken, retired.relayOrigin))
            },
            publishActive = { publishes++ },
        )

        assertEquals(
            RelayWakeReconcile.RETRY,
            coordinator(initial).reconcile(active.identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, active.bearerToken, true),
        )
        val failedReopen = RelayWakeStateStore(preferences.reopen(), box)
        assertFalse(requireNotNull(failedReopen.active).published)
        assertTrue(failedReopen.cleanups().isEmpty())
        assertEquals(0, retires)

        assertEquals(
            RelayWakeReconcile.REGISTERED,
            coordinator(RelayWakeStateStore(preferences, box)).reconcile(
                active.identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, active.bearerToken, true,
            ),
        )
        val recoveredReopen = RelayWakeStateStore(preferences.reopen(), box)
        assertTrue(requireNotNull(recoveredReopen.active).published)
        assertTrue(recoveredReopen.cleanups().isEmpty())
        assertEquals(2, publishes)
        assertEquals(0, retires)
    }

    @Test fun publicationAckCommitFailureJournalsExactAckWithoutRetiringCurrentRoute() {
        val preferences = MemoryMutatingPreferences()
        val active = RelayWakeActive(
            relayIdentity("fingerprint"), "https://relay.example", "hosted", "fingerprint",
            RelayRoute("00000000-0000-4000-8000-000000000096", "manage", "wake"), "bearer", false,
        )
        val initial = RelayWakeStateStore(preferences, plainBox)
        assertTrue(initial.saveActive(active))
        preferences.failCommits = true
        var publishes = 0
        var retires = 0
        fun coordinator(store: RelayWakeStateStore) = RelayWakeCoordinator(
            store,
            object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
            RelayRoutePublisher { _, _ -> },
            retire = { retired ->
                retires++
                store.saveCleanup(RelayWakeCleanup(retired.identity, retired.route, retired.bearerToken, retired.relayOrigin))
            },
            publishActive = { publishes++ },
        )

        assertEquals(
            RelayWakeReconcile.RETRY,
            coordinator(initial).reconcile(active.identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, active.bearerToken, true),
        )
        val failedReopen = RelayWakeStateStore(preferences.reopen(), plainBox)
        assertFalse(requireNotNull(failedReopen.active).published)
        assertTrue(failedReopen.cleanups().isEmpty())
        assertEquals(1, publishes)
        assertEquals(0, retires)

        preferences.failCommits = false
        assertEquals(
            RelayWakeReconcile.REGISTERED,
            coordinator(RelayWakeStateStore(preferences, plainBox)).reconcile(
                active.identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, active.bearerToken, true,
            ),
        )
        val recoveredReopen = RelayWakeStateStore(preferences.reopen(), plainBox)
        assertTrue(requireNotNull(recoveredReopen.active).published)
        assertTrue(recoveredReopen.cleanups().isEmpty())
        assertEquals(1, publishes)
        assertEquals(0, retires)
    }

    @Test fun cleanupDeleteFalseCommitRemainsQueuedUntilExactRemovalIsDurable() {
        val preferences = MemoryMutatingPreferences()
        val cleanup = RelayWakeCleanup(
            relayIdentity("fingerprint"),
            RelayRoute("00000000-0000-4000-8000-000000000101", "manage", "wake"),
            "bearer",
            "https://relay.example",
        )
        assertTrue(RelayWakeStateStore(preferences, plainBox).saveCleanup(cleanup))
        preferences.failCommits = true
        assertFalse(RelayWakeStateStore(preferences, plainBox).clearCleanup(cleanup.route.routeId))
        assertEquals(cleanup.route.routeId, RelayWakeStateStore(preferences.reopen(), plainBox).cleanups().single().route.routeId)
        assertFalse(RelayWakeStateStore(preferences, plainBox).recoverAcknowledgedState())

        preferences.failCommits = false
        assertTrue(RelayWakeStateStore(preferences, plainBox).recoverAcknowledgedState())
        assertTrue(RelayWakeStateStore(preferences.reopen(), plainBox).cleanups().isEmpty())
    }

    @Test fun earlyChallengeTransferPreservesSourceWhenPendingSealFailsThenRetries() {
        val preferences = MemoryMutatingPreferences()
        val box = SelectiveSealFailureBox()
        val state = RelayWakeStateStore(preferences, box)
        val challenge = "c".repeat(64)
        val pending = RelayWakePending(
            relayIdentity("fingerprint"), "https://relay.example", "hosted", "fingerprint",
            "00000000-0000-4000-8000-000000000111",
            RelayRoute("00000000-0000-4000-8000-000000000112", "manage", "wake"), null, 1L,
        )
        assertTrue(state.savePending(pending))
        assertTrue(state.saveEarlyChallenge(challenge))
        val coordinator = RelayWakeCoordinator(
            state,
            object : RelayWakeTransport { override fun post(path: String, body: String) = HttpResult(500, null) },
            RelayRoutePublisher { _, _ -> },
        )
        box.failPendingSeal = true

        assertFalse(RelayWakeRuntime.transferEarlyChallenge(state, coordinator))
        val failedReopen = RelayWakeStateStore(preferences.reopen(), box)
        assertEquals(challenge, failedReopen.earlyChallenge())
        assertNull(failedReopen.pending?.challenge)

        box.failPendingSeal = false
        assertTrue(RelayWakeRuntime.transferEarlyChallenge(state, coordinator))
        val recovered = RelayWakeStateStore(preferences.reopen(), box)
        assertNull(recovered.earlyChallenge())
        assertEquals(challenge, recovered.pending?.challenge)
    }

    @Test fun legacyRouteWithUnknownOriginStaysLocalAndDoesNotBlockSetup() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-legacy-unknown", 0)
        prefs.edit().clear().commit()
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000031", "manage", "wake"))
        val state = RelayWakeStateStore(prefs, plainBox)

        // Baseline disable removed the origin but kept the v1 slot: the gate succeeds so setup proceeds.
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L), null, legacy, state))

        assertFalse(legacy.cleared)
        assertTrue(state.cleanups().isEmpty())
        assertEquals("", state.legacyBinding())
    }

    @Test fun enablingAnotherRelayNeverQueuesUnknownOriginCredentialsToIt() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-legacy-enable-b", 0)
        prefs.edit().clear().commit()
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000032", "manage", "wake"))
        val state = RelayWakeStateStore(prefs, plainBox)
        val credential = GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L)
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, credential, null, legacy, state))

        // setEnabled(B): the new origin is written after the migration gate, then startup runs again.
        prefs.edit().putString("relay-origin.v1", "https://relay-b.example").commit()
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, credential, prefs.getString("relay-origin.v1", null), legacy, state))

        assertTrue(state.cleanups().isEmpty())
        assertFalse(legacy.cleared)
        assertEquals("", state.legacyBinding())
    }

    @Test fun knownOriginWithoutBearerKeepsItsOriginWhenTheRelayChanges() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val prefs = context.getSharedPreferences("relay-legacy-known-a", 0)
        prefs.edit().clear().putString("relay-origin.v1", "https://relay-a.example").commit()
        val legacy = FakeRouteStore(RelayRoute("00000000-0000-4000-8000-000000000033", "manage", "wake"))
        val state = RelayWakeStateStore(prefs, plainBox)

        // Signed out: origin A is recorded and the secret is kept; no cleanup can be created yet.
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, null, prefs.getString("relay-origin.v1", null), legacy, state))
        assertEquals("https://relay-a.example", state.legacyBinding())
        assertFalse(legacy.cleared)
        assertTrue(state.cleanups().isEmpty())

        // The user configures relay B, then a credential appears: cleanup still goes to A.
        prefs.edit().putString("relay-origin.v1", "https://relay-b.example").commit()
        val credential = GatewayAccountCredential("https://account.example", "vault", "device", "bearer", 1L)
        assertTrue(RelayWakeRuntime.migrateLegacyCustomRoute(prefs, credential, prefs.getString("relay-origin.v1", null), legacy, state))

        assertEquals("https://relay-a.example", state.cleanups().single().relayOrigin)
        assertTrue(legacy.cleared)
        assertNull(state.legacyBinding())
    }
}

private class TemporarilyUnreadableBox : SecretBox {
    var readable = true
    override fun seal(purpose: String, plaintext: ByteArray) = "sealed:$purpose:${plaintext.decodeToString()}"
    override fun open(purpose: String, sealed: String): ByteArray? = if (readable) {
        sealed.removePrefix("sealed:$purpose:").takeIf { it != sealed }?.encodeToByteArray()
    } else null
}

private class PublishAckLatchBox : SecretBox {
    val ackSealEntered = CountDownLatch(1)
    val archiveSealEntered = CountDownLatch(1)
    val releaseAckSeal = CountDownLatch(1)
    override fun seal(purpose: String, plaintext: ByteArray): String {
        val value = plaintext.decodeToString()
        if (purpose.startsWith("hosted-relay-cleanup.v2.")) archiveSealEntered.countDown()
        if (purpose == "hosted-relay-active-binding.v2" && value.endsWith("\ntrue")) {
            ackSealEntered.countDown()
            check(releaseAckSeal.await(5, TimeUnit.SECONDS))
        }
        return value
    }
    override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
}

private class OneShotPublicationSealFailureBox : SecretBox {
    private var remainingFailures = 1

    override fun seal(purpose: String, plaintext: ByteArray): String {
        val value = plaintext.decodeToString()
        if (purpose == "hosted-relay-active-binding.v2" && value.endsWith("\ntrue") && remainingFailures > 0) {
            remainingFailures--
            throw IllegalStateException("publication acknowledgement seal unavailable")
        }
        return value
    }

    override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
}

private class SelectiveSealFailureBox : SecretBox {
    var failPendingSeal = false
    override fun seal(purpose: String, plaintext: ByteArray): String {
        if (failPendingSeal && purpose == "hosted-relay-pending.v2") throw IllegalStateException("pending seal unavailable")
        return plaintext.decodeToString()
    }
    override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
}

private class MemoryMutatingPreferences(
    initialDurable: Map<String, Any?> = emptyMap(),
) : SharedPreferences {
    private val memory = initialDurable.toMutableMap()
    private val durable = initialDurable.toMutableMap()
    var failCommits = false
    var successfulCommitsBeforeFailure: Int? = null

    fun reopen() = MemoryMutatingPreferences(durable)

    override fun getAll(): MutableMap<String, *> = memory.toMutableMap()
    override fun getString(key: String, defaultValue: String?): String? = memory[key] as? String ?: defaultValue
    override fun getStringSet(key: String, defaultValues: MutableSet<String>?): MutableSet<String>? =
        @Suppress("UNCHECKED_CAST") ((memory[key] as? MutableSet<String>) ?: defaultValues)
    override fun getInt(key: String, defaultValue: Int) = memory[key] as? Int ?: defaultValue
    override fun getLong(key: String, defaultValue: Long) = memory[key] as? Long ?: defaultValue
    override fun getFloat(key: String, defaultValue: Float) = memory[key] as? Float ?: defaultValue
    override fun getBoolean(key: String, defaultValue: Boolean) = memory[key] as? Boolean ?: defaultValue
    override fun contains(key: String) = memory.containsKey(key)
    override fun edit(): SharedPreferences.Editor = Editor()
    override fun registerOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) = Unit
    override fun unregisterOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) = Unit

    private inner class Editor : SharedPreferences.Editor {
        private val updates = mutableMapOf<String, Any?>()
        private val removals = mutableSetOf<String>()
        private var clear = false

        override fun putString(key: String, value: String?) = apply { updates[key] = value; removals.remove(key) }
        override fun putStringSet(key: String, values: MutableSet<String>?) = apply { updates[key] = values; removals.remove(key) }
        override fun putInt(key: String, value: Int) = apply { updates[key] = value; removals.remove(key) }
        override fun putLong(key: String, value: Long) = apply { updates[key] = value; removals.remove(key) }
        override fun putFloat(key: String, value: Float) = apply { updates[key] = value; removals.remove(key) }
        override fun putBoolean(key: String, value: Boolean) = apply { updates[key] = value; removals.remove(key) }
        override fun remove(key: String) = apply { removals += key; updates.remove(key) }
        override fun clear() = apply { clear = true; updates.clear(); removals.clear() }
        override fun apply() { commit() }
        override fun commit(): Boolean {
            if (clear) memory.clear()
            removals.forEach { key -> memory.remove(key) }
            updates.forEach { (key, value) -> if (value == null) memory.remove(key) else memory[key] = value }
            val remaining = successfulCommitsBeforeFailure
            val committed = !failCommits && (remaining == null || remaining > 0)
            if (remaining != null && remaining > 0) successfulCommitsBeforeFailure = remaining - 1
            if (committed) { durable.clear(); durable.putAll(memory) }
            return committed
        }
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
