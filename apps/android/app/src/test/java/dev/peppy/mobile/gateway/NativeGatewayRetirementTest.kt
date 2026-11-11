package dev.peppy.mobile.gateway

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.WorkerParameters
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import dev.peppy.mobile.RelayRoute
import dev.peppy.mobile.RelayRoutePublisher
import dev.peppy.mobile.RelayWakeRuntime
import dev.peppy.mobile.RelayWakeReconcileWorker
import dev.peppy.mobile.RelayWakeWorkerDispatch
import dev.peppy.mobile.RelayWakeWorkerEnrollmentInput
import dev.peppy.mobile.relayWakeWorkerEnrollmentInputFactory
import dev.peppy.mobile.RelayWakeTransport
import dev.peppy.mobile.relayWakeStateStoreFactory
import dev.peppy.mobile.relay.RelayWakeActive
import dev.peppy.mobile.relay.RelayWakeCoordinator
import dev.peppy.mobile.relay.RelayWakeIdentity
import dev.peppy.mobile.relay.RelayWakeCleanup
import dev.peppy.mobile.relay.RelayWakePending
import dev.peppy.mobile.relay.RelayWakeReconcile
import dev.peppy.mobile.relay.RelayWakeStateStore
import dev.peppy.mobile.security.SecretBox
import dev.peppy.mobile.security.VaultMaterial
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.peppy_mobile_bindings.NativeClient

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class NativeGatewayRetirementTest : GatewayTestBase() {
    @Before
    fun initializeWorkManager() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun resetRetirementFixtures() {
        NativeGateway.persistenceDecisionHook = null
        NativeGateway.relayArchiveHook = null
        RelayWakeRuntime.beforeApplyCurrentRelayRuntimeHook = null
        relayWakeWorkerEnrollmentInputFactory = null
        HistoricalRelayReconcileWorker.dispatch = null
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun closeForTestRevokesCapturedTicketBeforeRetiringItsOwner() {
        enrollAndUnlock()
        var revocations = 0
        val lease = NativeGateway.acquireRcsSendLease(context, "ticket-1") { revocations++ }

        assertNotNull(lease)
        NativeGateway.closeForTest()

        assertEquals(1, revocations)
        assertNull(NativeGateway.acquireRcsSendLease(context, "ticket-2") {})
        assertNotNull(NativeGateway.open(context))
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "ticket-2") {})
        lease!!.close()
    }

    @Test
    fun retirementListenerRunsWhenTheGatewaySessionRetires() {
        enrollAndUnlock()
        val notifications = AtomicInteger()
        val unregister = NativeGateway.addRetirementListener { notifications.incrementAndGet() }

        NativeGateway.closeForTest()

        assertEquals(1, notifications.get())
        unregister()
    }

    @Test
    fun retirementListenerRegistrationDoesNotWaitForOwnerLock() {
        enrollAndUnlock()
        val decisionEntered = CountDownLatch(1)
        val releaseDecision = CountDownLatch(1)
        val firstHook = AtomicBoolean(true)
        NativeGateway.persistenceDecisionHook = {
            if (firstHook.getAndSet(false)) {
                decisionEntered.countDown()
                check(releaseDecision.await(5, TimeUnit.SECONDS))
            }
        }
        // The decision hook runs while the owner lock is held, like SQLCipher open under that lock.
        val persistThread = Thread { NativeGateway.persistVerified(context, parsed(), vaultMaterial()) }
        val registration = AtomicReference<() -> Unit>()
        val registrar = Thread { registration.set(NativeGateway.addRetirementListener {}) }
        try {
            persistThread.start()
            assertTrue(decisionEntered.await(5, TimeUnit.SECONDS))

            registrar.start()
            registrar.join(2_000)
            assertFalse("listener registration must not wait for the owner lock", registrar.isAlive)
            assertNotNull(registration.get())
            registration.get().invoke()
        } finally {
            releaseDecision.countDown()
            persistThread.join(5_000)
            registrar.join(5_000)
        }
    }

    @Test
    fun archiveDoesNotHoldNativeOwnerLockWhileRelayPublicationHoldsTheJournal() {
        enrollAndUnlock()
        val credential = checkNotNull(NativeGateway.accountCredential(context))
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        val box = object : SecretBox {
            override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString()
            override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
        }
        val previousStoreFactory = relayWakeStateStoreFactory
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }
        val state = RelayWakeStateStore(preferences, box)
        val identity = RelayWakeIdentity(
            credential.origin,
            credential.vaultId,
            credential.deviceId,
            "fcm",
            "production",
            state.tokenFingerprint(credential.bearerToken),
        )
        val active = RelayWakeActive(
            identity,
            "https://relay.example",
            "hosted",
            "device-token",
            RelayRoute("00000000-0000-4000-8000-000000000101", "manage", "wake"),
            credential.bearerToken,
            false,
        )
        assertTrue(state.saveActive(active))
        val publicationEntered = CountDownLatch(1)
        val releasePublication = CountDownLatch(1)
        val archiveRelayEntered = CountDownLatch(1)
        val releaseArchiveRelay = CountDownLatch(1)
        NativeGateway.relayArchiveHook = {
            archiveRelayEntered.countDown()
            check(releaseArchiveRelay.await(5, TimeUnit.SECONDS))
        }
        val coordinator = RelayWakeCoordinator(
            RelayWakeStateStore(preferences, box),
            object : RelayWakeTransport { override fun post(path: String, body: String) = error("unexpected relay request") },
            RelayRoutePublisher { _, _ -> },
            publishActive = {
                publicationEntered.countDown()
                check(releasePublication.await(5, TimeUnit.SECONDS))
                checkNotNull(NativeGateway.accountCredential(context))
            },
        )
        val publishResult = AtomicReference<RelayWakeReconcile>()
        val publishFailure = AtomicReference<Throwable>()
        val publishThread = Thread {
            runCatching {
                coordinator.reconcile(identity, active.relayOrigin, active.protocol, "device", active.tokenFingerprint, credential.bearerToken, true)
            }.onSuccess(publishResult::set).onFailure(publishFailure::set)
        }
        val archiveResult = AtomicReference<Boolean>()
        val archiveFailure = AtomicReference<Throwable>()
        val archiveThread = Thread { runCatching { NativeGateway.archiveEnrollment(context) }.onSuccess(archiveResult::set).onFailure(archiveFailure::set) }
        try {
            publishThread.start()
            assertTrue(publicationEntered.await(5, TimeUnit.SECONDS))
            archiveThread.start()
            assertTrue(archiveRelayEntered.await(5, TimeUnit.SECONDS))

            releasePublication.countDown()
            publishThread.join(2_000)
            assertFalse("publication must not wait on the native owner monitor", publishThread.isAlive)
            assertNull(publishFailure.get())
            assertEquals(RelayWakeReconcile.REGISTERED, publishResult.get())

            releaseArchiveRelay.countDown()
            archiveThread.join(5_000)
            assertFalse(archiveThread.isAlive)
            assertNull(archiveFailure.get())
            assertTrue(archiveResult.get())
            val archived = RelayWakeStateStore(preferences, box)
            assertNull(archived.active)
            assertEquals(active.route.routeId, archived.cleanups().single().route.routeId)
        } finally {
            releasePublication.countDown()
            releaseArchiveRelay.countDown()
            publishThread.join(5_000)
            archiveThread.join(5_000)
            relayWakeStateStoreFactory = previousStoreFactory
        }
    }

    @Test
    fun archiveCompletingAtFinalRelayInitializationBoundaryPreventsWakeResurrection() {
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        val box = object : SecretBox {
            override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString()
            override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
        }
        val previousStoreFactory = relayWakeStateStoreFactory
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }
        val hostedCredential = parsed(origin = "https://peppy.pro")
        assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, hostedCredential, vaultMaterial()))
        assertTrue(RelayWakeRuntime.enabled)
        assertNotNull(RelayWakeRuntime.client)

        val finalApplyEntered = CountDownLatch(1)
        val releaseFinalApply = CountDownLatch(1)
        RelayWakeRuntime.beforeApplyCurrentRelayRuntimeHook = {
            finalApplyEntered.countDown()
            check(releaseFinalApply.await(5, TimeUnit.SECONDS))
        }
        val initializationResult = AtomicReference<ImportResult>()
        val initializationFailure = AtomicReference<Throwable>()
        val initializationThread = Thread {
            runCatching {
                NativeGateway.persistVerified(context, hostedCredential, vaultMaterial())
            }.onSuccess(initializationResult::set).onFailure(initializationFailure::set)
        }
        val archiveResult = AtomicReference<Boolean>()
        val archiveFailure = AtomicReference<Throwable>()
        val archiveThread = Thread {
            runCatching { NativeGateway.archiveEnrollment(context) }
                .onSuccess(archiveResult::set)
                .onFailure(archiveFailure::set)
        }

        try {
            initializationThread.start()
            assertTrue(finalApplyEntered.await(5, TimeUnit.SECONDS))
            archiveThread.start()
            archiveThread.join(5_000)
            assertFalse("archive completes while initialization waits outside the native fence", archiveThread.isAlive)
            assertNull(archiveFailure.get())
            assertEquals(true, archiveResult.get())

            releaseFinalApply.countDown()
            initializationThread.join(5_000)
            assertFalse(initializationThread.isAlive)
            assertNull(initializationFailure.get())
            assertEquals(ImportResult.UPDATED, initializationResult.get())
            assertNull(NativeGateway.accountCredential(context))
            assertNull(RelayWakeRuntime.client)
            assertFalse(RelayWakeRuntime.enabled)
            assertFalse(RelayWakeRuntime.acceptsWakes(context))

            RelayWakeRuntime.challenge(context, "f".repeat(64))
            assertNull(RelayWakeStateStore(preferences, box).earlyChallenge())
        } finally {
            releaseFinalApply.countDown()
            initializationThread.join(5_000)
            archiveThread.join(5_000)
            RelayWakeRuntime.beforeApplyCurrentRelayRuntimeHook = null
            relayWakeStateStoreFactory = previousStoreFactory
        }
    }

    @Test
    fun archiveReadFailureThenReplacementRetiresAndDrainsCapturedPendingRoute() {
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        val box = ToggleReadableBox()
        val previousStoreFactory = relayWakeStateStoreFactory
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }
        val bearerA = "aa".repeat(48)
        val bearerB = "bb".repeat(48)
        val credentialA = parsed(origin = "https://peppy.pro", token = bearerA)
        val credentialB = parsed(origin = "https://peppy.pro", token = bearerB)
        val routeA = RelayRoute("00000000-0000-4000-8000-000000000131", "manage-a", "wake-a")
        val relayOriginA = "https://relay-a.example"

        try {
            assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credentialA, vaultMaterial()))
            val accountA = checkNotNull(NativeGateway.accountCredential(context))
            val stateA = RelayWakeStateStore(preferences, box)
            val identityA = RelayWakeIdentity(
                accountA.origin,
                accountA.vaultId,
                accountA.deviceId,
                "fcm",
                "production",
                stateA.tokenFingerprint(accountA.bearerToken),
            )
            assertTrue(
                stateA.savePending(
                    RelayWakePending(
                        identityA,
                        relayOriginA,
                        "hosted",
                        stateA.tokenFingerprint("device-token-a"),
                        "00000000-0000-4000-8000-000000000130",
                        routeA,
                        "f".repeat(64),
                        1L,
                    ),
                ),
            )

            box.readable = false
            assertTrue(NativeGateway.archiveEnrollment(context))
            assertNull(NativeGateway.accountCredential(context))
            assertTrue(preferences.contains("hosted-relay-pending.v2"))

            assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credentialB, vaultMaterial()))
            assertEquals(bearerB, checkNotNull(NativeGateway.accountCredential(context)).bearerToken)
            assertTrue(preferences.contains("hosted-relay-pending.v2"))

            box.readable = true
            RelayWakeRuntime.initialize(context)
            val dispatch = HistoricalCleanupDispatch(listOf(HttpResult(500, null), HttpResult(200, null)))
            HistoricalRelayReconcileWorker.dispatch = dispatch
            relayWakeWorkerEnrollmentInputFactory = {
                RelayWakeWorkerEnrollmentInput(
                    deviceToken = "device-token-b",
                    transport = object : RelayWakeTransport {
                        override fun post(path: String, body: String) = HttpResult(500, null)
                    },
                    supportsCandidates = false,
                )
            }

            assertEquals(ListenableWorker.Result.retry(), runHistoricalRelayWorker())
            val retiredA = RelayWakeStateStore(preferences, box).cleanups().single()
            assertEquals(routeA.routeId, retiredA.route.routeId)
            assertNull(retiredA.bearerToken)
            assertEquals(relayOriginA, retiredA.relayOrigin)
            assertEquals("manage-a", retiredA.route.manageCredential)
            assertEquals(bearerB, checkNotNull(NativeGateway.accountCredential(context)).bearerToken)

            assertEquals(ListenableWorker.Result.retry(), runHistoricalRelayWorker())
            assertTrue(dispatch.accountDeletes.isEmpty())
            assertEquals(listOf(relayOriginA to "manage-a"), dispatch.revocations)
            assertEquals(routeA.routeId, RelayWakeStateStore(preferences, box).cleanups().single().route.routeId)

            assertEquals(ListenableWorker.Result.retry(), runHistoricalRelayWorker())
            assertTrue(dispatch.accountDeletes.isEmpty())
            assertEquals(listOf(relayOriginA to "manage-a", relayOriginA to "manage-a"), dispatch.revocations)
            assertTrue(RelayWakeStateStore(preferences, box).cleanups().isEmpty())
            assertEquals(bearerB, checkNotNull(NativeGateway.accountCredential(context)).bearerToken)
            assertFalse(dispatch.revocations.any { it.first != relayOriginA })
        } finally {
            box.readable = true
            relayWakeWorkerEnrollmentInputFactory = null
            HistoricalRelayReconcileWorker.dispatch = null
            relayWakeStateStoreFactory = previousStoreFactory
        }
    }

    private fun runHistoricalRelayWorker() =
        TestListenableWorkerBuilder<HistoricalRelayReconcileWorker>(context)
            .build()
            .startWork()
            .get(5, TimeUnit.SECONDS)

    @Test
    fun failedRevocationKeepsOwnerUnavailableUntilSameRetirementRetriesSuccessfully() {
        enrollAndUnlock()
        val firstCalls = AtomicInteger()
        val secondCalls = AtomicInteger()
        val retryEntered = CountDownLatch(1)
        val releaseRetry = CountDownLatch(1)
        val firstMustFail = AtomicBoolean(true)
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "ticket-failing") {
            firstCalls.incrementAndGet()
            if (firstMustFail.get()) throw IllegalStateException("injected revoke failure")
            retryEntered.countDown()
            check(releaseRetry.await(5, TimeUnit.SECONDS))
        })
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "ticket-second") {
            secondCalls.incrementAndGet()
        })

        val archiveResult = AtomicReference<Boolean>()
        val archiveThread = Thread { archiveResult.set(NativeGateway.archiveEnrollment(context)) }
        val persistStarted = CountDownLatch(1)
        val persistFinished = CountDownLatch(1)
        val persistResult = AtomicReference<ImportResult>()
        val persistThread = Thread {
            persistStarted.countDown()
            persistResult.set(NativeGateway.persistVerified(context, parsed(token = "ab".repeat(48)), vaultMaterial()))
            persistFinished.countDown()
        }
        try {
            assertFalse(NativeGateway.archiveEnrollment(context))
            assertEquals(1, firstCalls.get())
            assertEquals("all revokers run after one throws", 1, secondCalls.get())
            assertNull(NativeGateway.open(context))
            assertNull(NativeGateway.acquireRcsSendLease(context, "ticket-blocked") {})

            NativeGateway.closeForTest()
            assertEquals("test cleanup cannot bypass failed revocation", 2, firstCalls.get())
            assertEquals(2, secondCalls.get())
            assertNull(NativeGateway.open(context))

            firstMustFail.set(false)
            archiveThread.start()
            assertTrue(retryEntered.await(5, TimeUnit.SECONDS))
            persistThread.start()
            assertTrue(persistStarted.await(5, TimeUnit.SECONDS))
            assertFalse("competing mutation waits for the retry", persistFinished.await(100, TimeUnit.MILLISECONDS))
            assertNull(NativeGateway.open(context))
            assertNull(NativeGateway.acquireRcsSendLease(context, "ticket-still-blocked") {})

            releaseRetry.countDown()
            archiveThread.join(5_000)
            persistThread.join(5_000)
            assertFalse(archiveThread.isAlive)
            assertFalse(persistThread.isAlive)
            assertEquals(true, archiveResult.get())
            assertEquals(ImportResult.IMPORTED, persistResult.get())
            assertEquals(3, firstCalls.get())
            assertEquals(3, secondCalls.get())
            assertNotNull(NativeGateway.open(context))
        } finally {
            firstMustFail.set(false)
            releaseRetry.countDown()
            if (archiveThread.isAlive) archiveThread.join(5_000)
            if (persistThread.isAlive) persistThread.join(5_000)
        }
    }

    @Test
    fun unchangedPersistenceCommitExcludesConcurrentTokenRotation() {
        enrollAndUnlock()
        val currentLease = checkNotNull(NativeGateway.acquireRcsSendLease(context, "current-ticket") {})
        val decisionEntered = CountDownLatch(1)
        val releaseDecision = CountDownLatch(1)
        val firstHook = AtomicBoolean(true)
        NativeGateway.persistenceDecisionHook = {
            if (firstHook.getAndSet(false)) {
                decisionEntered.countDown()
                check(releaseDecision.await(5, TimeUnit.SECONDS))
            }
        }
        val unchangedResult = AtomicReference<ImportResult>()
        val unchangedThread = Thread {
            unchangedResult.set(NativeGateway.persistVerified(context, parsed(), vaultMaterial()))
        }
        val successorReady = CountDownLatch(1)
        val successorRevocations = AtomicInteger()
        val rotationThread = Thread {
            check(NativeGateway.persistVerified(context, parsed(token = "bc".repeat(48)), vaultMaterial()) == ImportResult.UPDATED)
            checkNotNull(NativeGateway.acquireRcsSendLease(context, "successor-ticket") {
                successorRevocations.incrementAndGet()
            })
            successorReady.countDown()
        }
        try {
            unchangedThread.start()
            assertTrue(decisionEntered.await(5, TimeUnit.SECONDS))
            rotationThread.start()
            assertFalse("rotation cannot pass an unchanged commit boundary", successorReady.await(100, TimeUnit.MILLISECONDS))
            releaseDecision.countDown()
            unchangedThread.join(5_000)
            rotationThread.join(5_000)
            assertFalse(unchangedThread.isAlive)
            assertFalse(rotationThread.isAlive)
            assertEquals(ImportResult.UPDATED, unchangedResult.get())
            assertTrue(successorReady.await(5, TimeUnit.SECONDS))

            assertEquals(
                ImportResult.UPDATED,
                NativeGateway.persistVerified(context, parsed(token = "cd".repeat(48)), vaultMaterial()),
            )
            assertEquals("successor lease remains registered at the current generation", 1, successorRevocations.get())
        } finally {
            NativeGateway.persistenceDecisionHook = null
            releaseDecision.countDown()
            if (unchangedThread.isAlive) unchangedThread.join(5_000)
            if (rotationThread.isAlive) rotationThread.join(5_000)
            currentLease.close()
        }
    }

    @Test
    fun visibleCommitFailureRetiresOldClientAndRequiresExplicitRetry() {
        enrollAndUnlock()
        val oldClient = checkNotNull(NativeGateway.open(context))
        val oldRevocations = AtomicInteger()
        val oldLease = checkNotNull(NativeGateway.acquireRcsSendLease(context, "old-ticket") {
            oldRevocations.incrementAndGet()
        })
        val rotated = uniffi.peppy_mobile_bindings.createVaultMaterial(vaultId, TEST_PASSPHRASE)
        val rotatedMaterial = VaultMaterial(rotated.profileJson, rotated.headerJson)
        val failingContext = CommitFailingContext(context).apply { failNextCommit.set(true) }

        assertEquals(
            ImportResult.STORAGE_ERROR,
            NativeGateway.persistVerified(failingContext, parsed(), rotatedMaterial),
        )
        assertEquals(1, oldRevocations.get())
        val visibleProfile = context.getSharedPreferences("peppy-secure", Context.MODE_PRIVATE)
            .getString("profile.v1", null)
            ?.let { NativeGateway.secretBox.open("profile.v1", it) }
            ?.toString(Charsets.UTF_8)
        assertEquals("failed disk commit still updated SharedPreferences memory", rotated.profileJson, visibleProfile)
        assertNull(NativeGateway.open(context))
        assertNull(NativeGateway.acquireRcsSendLease(context, "blocked-after-commit-failure") {})
        NativeGateway.closeForTest()
        assertNull("test cleanup cannot bypass a failed durable commit", NativeGateway.open(context))

        assertEquals(
            ImportResult.IDENTITY_MISMATCH,
            NativeGateway.persistVerified(context, parsed(), rotatedMaterial) { false },
        )
        assertNull("cancelled recovery keeps the durable-commit fence", NativeGateway.open(context))
        assertNull(NativeGateway.acquireRcsSendLease(context, "blocked-after-cancelled-retry") {})
        NativeGateway.closeForTest()
        assertNull(NativeGateway.open(context))

        failingContext.failNextCommit.set(true)
        assertEquals(
            ImportResult.STORAGE_ERROR,
            NativeGateway.persistVerified(failingContext, parsed(), rotatedMaterial),
        )
        assertNull("same-material commit failure keeps the durable-commit fence", NativeGateway.open(context))
        assertNull(NativeGateway.acquireRcsSendLease(context, "blocked-after-second-commit-failure") {})
        NativeGateway.closeForTest()
        assertNull(NativeGateway.open(context))

        val failedRetryEntered = CountDownLatch(1)
        val releaseFailedRetry = CountDownLatch(1)
        val failedRetryResult = AtomicReference<ImportResult>()
        val failedRetryThread = Thread {
            failedRetryResult.set(
                NativeGateway.persistVerified(context, parsed(), rotatedMaterial) {
                    failedRetryEntered.countDown()
                    check(releaseFailedRetry.await(5, TimeUnit.SECONDS))
                    false
                },
            )
        }
        val contenderFinished = CountDownLatch(1)
        val contenderResult = AtomicReference<ImportResult>()
        val contenderThread = Thread {
            contenderResult.set(NativeGateway.persistVerified(context, parsed(), rotatedMaterial))
            contenderFinished.countDown()
        }
        try {
            failedRetryThread.start()
            assertTrue(failedRetryEntered.await(5, TimeUnit.SECONDS))
            contenderThread.start()
            assertFalse("contender waits for the active recovery attempt", contenderFinished.await(100, TimeUnit.MILLISECONDS))
        } finally {
            releaseFailedRetry.countDown()
            failedRetryThread.join(5_000)
            contenderThread.join(5_000)
        }
        assertFalse(failedRetryThread.isAlive)
        assertFalse(contenderThread.isAlive)
        assertEquals(ImportResult.IDENTITY_MISMATCH, failedRetryResult.get())
        assertEquals("restored failed fence wakes a successful contender", ImportResult.UPDATED, contenderResult.get())

        val successorClient = checkNotNull(NativeGateway.open(context))
        assertFalse("retry must not retain the old unlocked client", successorClient === oldClient)
        val successorRevocations = AtomicInteger()
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "successor-ticket") {
            successorRevocations.incrementAndGet()
        })
        oldLease.close()
        oldLease.close()

        assertEquals(
            ImportResult.UPDATED,
            NativeGateway.persistVerified(context, parsed(token = "de".repeat(48)), rotatedMaterial),
        )
        assertEquals("stale lease close cannot unregister the successor", 1, successorRevocations.get())
    }


    @Test
    fun tokenAndProfileReplacementFenceAcquisitionWhileCallbacksAreHeld() {
        enrollAndUnlock()

        fun replaceWhileHeld(
            ticket: String,
            token: String,
            material: VaultMaterial,
        ): Pair<NativeClient, NativeClient> {
            val before = checkNotNull(NativeGateway.open(context))
            val callbackEntered = CountDownLatch(1)
            val releaseCallback = CountDownLatch(1)
            assertNotNull(NativeGateway.acquireRcsSendLease(context, ticket) {
                assertNull(NativeGateway.open(context))
                callbackEntered.countDown()
                check(releaseCallback.await(5, TimeUnit.SECONDS))
            })
            val result = AtomicReference<ImportResult>()
            val thread = Thread {
                result.set(NativeGateway.persistVerified(context, parsed(token = token), material))
            }
            try {
                thread.start()
                assertTrue(callbackEntered.await(5, TimeUnit.SECONDS))
                assertNull(NativeGateway.open(context))
                assertNull(NativeGateway.acquireRcsSendLease(context, "$ticket-blocked") {})
            } finally {
                releaseCallback.countDown()
                thread.join(5_000)
            }
            assertFalse(thread.isAlive)
            assertEquals(ImportResult.UPDATED, result.get())
            return before to checkNotNull(NativeGateway.open(context))
        }

        val tokenClients = replaceWhileHeld("token-ticket", "12".repeat(48), vaultMaterial())
        assertTrue("token-only replacement keeps the native database owner", tokenClients.first === tokenClients.second)

        val rotated = uniffi.peppy_mobile_bindings.createVaultMaterial(vaultId, TEST_PASSPHRASE)
        val profileClients = replaceWhileHeld(
            "profile-ticket",
            "12".repeat(48),
            VaultMaterial(rotated.profileJson, rotated.headerJson),
        )
        assertFalse("profile replacement retires the old native owner", profileClients.first === profileClients.second)
    }

    @Test
    fun waitingPersistenceRechecksBeforeCommitAndArchiveRevalidatesAfterPersistence() {
        enrollAndUnlock()
        val firstCallbackEntered = CountDownLatch(1)
        val releaseFirstCallback = CountDownLatch(1)
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "first-persistence-ticket") {
            firstCallbackEntered.countDown()
            check(releaseFirstCallback.await(5, TimeUnit.SECONDS))
        })
        val firstResult = AtomicReference<ImportResult>()
        val firstThread = Thread {
            firstResult.set(NativeGateway.persistVerified(context, parsed(token = "23".repeat(48)), vaultMaterial()))
        }
        val allowCommit = AtomicBoolean(true)
        val beforeCommitCalled = CountDownLatch(1)
        val waitingResult = AtomicReference<ImportResult>()
        val waitingThread = Thread {
            waitingResult.set(
                NativeGateway.persistVerified(context, parsed(token = "34".repeat(48)), vaultMaterial()) {
                    beforeCommitCalled.countDown()
                    allowCommit.get()
                },
            )
        }
        try {
            firstThread.start()
            assertTrue(firstCallbackEntered.await(5, TimeUnit.SECONDS))
            waitingThread.start()
            assertFalse("beforeCommit is evaluated only after retirement waiting", beforeCommitCalled.await(100, TimeUnit.MILLISECONDS))
            allowCommit.set(false)
        } finally {
            releaseFirstCallback.countDown()
            firstThread.join(5_000)
            waitingThread.join(5_000)
        }
        assertFalse(firstThread.isAlive)
        assertFalse(waitingThread.isAlive)
        assertEquals(ImportResult.UPDATED, firstResult.get())
        assertEquals(ImportResult.IDENTITY_MISMATCH, waitingResult.get())
        assertEquals(0L, beforeCommitCalled.count)

        val secondCallbackEntered = CountDownLatch(1)
        val releaseSecondCallback = CountDownLatch(1)
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "profile-before-archive") {
            secondCallbackEntered.countDown()
            check(releaseSecondCallback.await(5, TimeUnit.SECONDS))
        })
        val rotated = uniffi.peppy_mobile_bindings.createVaultMaterial(vaultId, TEST_PASSPHRASE)
        val profileResult = AtomicReference<ImportResult>()
        val profileThread = Thread {
            profileResult.set(
                NativeGateway.persistVerified(
                    context,
                    parsed(token = "23".repeat(48)),
                    VaultMaterial(rotated.profileJson, rotated.headerJson),
                ),
            )
        }
        val archiveFinished = CountDownLatch(1)
        val archiveResult = AtomicReference<Boolean>()
        val archiveThread = Thread {
            archiveResult.set(NativeGateway.archiveEnrollment(context))
            archiveFinished.countDown()
        }
        try {
            profileThread.start()
            assertTrue(secondCallbackEntered.await(5, TimeUnit.SECONDS))
            archiveThread.start()
            assertFalse("archive waits for persistence retirement", archiveFinished.await(100, TimeUnit.MILLISECONDS))
        } finally {
            releaseSecondCallback.countDown()
            profileThread.join(5_000)
            archiveThread.join(5_000)
        }
        assertFalse(profileThread.isAlive)
        assertFalse(archiveThread.isAlive)
        assertEquals(ImportResult.UPDATED, profileResult.get())
        assertEquals(true, archiveResult.get())
        assertNull(NativeGateway.accountCredential(context))
    }


    @Test
    fun archiveCopyFailureRetiresCapturedOwnerAndRemainsResumable() {
        enrollAndUnlock()
        val capturedClient = checkNotNull(NativeGateway.open(context))
        val revocations = AtomicInteger()
        assertNotNull(NativeGateway.acquireRcsSendLease(context, "archive-copy-failure") {
            revocations.incrementAndGet()
        })
        val archiveParent = context.noBackupFilesDir.resolve("peppy-archives")
        assertTrue(archiveParent.createNewFile())

        assertFalse(NativeGateway.archiveEnrollment(context))
        assertEquals(1, revocations.get())
        assertNull(NativeGateway.acquireRcsSendLease(context, "no-half-retired-owner") {})
        val recoveredClient = checkNotNull(NativeGateway.open(context))
        assertFalse(capturedClient === recoveredClient)

        assertTrue(archiveParent.delete())
        assertTrue(NativeGateway.archiveEnrollment(context))
        assertNull(NativeGateway.accountCredential(context))
    }

    private class ToggleReadableBox : SecretBox {
        @Volatile var readable = true
        override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString()
        override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray().takeIf { readable }
    }

    private class HistoricalCleanupDispatch(private val revokeResults: List<HttpResult>) : RelayWakeWorkerDispatch {
        val accountDeletes = mutableListOf<Triple<String, String, String>>()
        val revocations = mutableListOf<Pair<String, String>>()
        private var revokeIndex = 0

        override fun deleteWakeRoute(cleanup: RelayWakeCleanup, bearerToken: String): HttpResult {
            accountDeletes += Triple(cleanup.identity.origin, bearerToken, cleanup.route.routeId)
            return HttpResult(200, null)
        }

        override fun revokeRoute(cleanup: RelayWakeCleanup): HttpResult {
            revocations += cleanup.relayOrigin to cleanup.route.manageCredential
            return revokeResults[revokeIndex++]
        }
    }

    class HistoricalRelayReconcileWorker(context: Context, params: WorkerParameters) :
        RelayWakeReconcileWorker(context, params, checkNotNull(dispatch)) {
        companion object {
            internal var dispatch: RelayWakeWorkerDispatch? = null
        }
    }

    private class CommitFailingContext(base: Context) : ContextWrapper(base) {
        val failNextCommit = AtomicBoolean()

        override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            val preferences = super.getSharedPreferences(name, mode)
            if (name != "peppy-secure") return preferences
            return object : SharedPreferences by preferences {
                override fun edit(): SharedPreferences.Editor = FailingEditor(preferences.edit(), failNextCommit)
            }
        }
    }

    private class FailingEditor(
        private val delegate: SharedPreferences.Editor,
        private val failNextCommit: AtomicBoolean,
    ) : SharedPreferences.Editor {
        override fun putString(key: String, value: String?) = apply { delegate.putString(key, value) }
        override fun putStringSet(key: String, values: MutableSet<String>?) = apply { delegate.putStringSet(key, values) }
        override fun putInt(key: String, value: Int) = apply { delegate.putInt(key, value) }
        override fun putLong(key: String, value: Long) = apply { delegate.putLong(key, value) }
        override fun putFloat(key: String, value: Float) = apply { delegate.putFloat(key, value) }
        override fun putBoolean(key: String, value: Boolean) = apply { delegate.putBoolean(key, value) }
        override fun remove(key: String) = apply { delegate.remove(key) }
        override fun clear() = apply { delegate.clear() }
        override fun apply() = delegate.apply()
        override fun commit(): Boolean {
            val committed = delegate.commit()
            return if (failNextCommit.getAndSet(false)) false else committed
        }
    }

}
