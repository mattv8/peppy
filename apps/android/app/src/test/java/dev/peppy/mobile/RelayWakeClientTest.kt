package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.ListenableWorker
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.TestListenableWorkerBuilder
import androidx.work.testing.WorkManagerTestInitHelper
import dev.peppy.mobile.gateway.HttpResult
import dev.peppy.mobile.relay.RelayWakeCleanup
import dev.peppy.mobile.relay.RelayWakeIdentity
import dev.peppy.mobile.relay.RelayWakeReconcile
import dev.peppy.mobile.relay.RelayWakeStateStore
import dev.peppy.mobile.security.SecretBox
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import java.util.concurrent.TimeUnit
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RelayWakeClientTest {
    @Test fun enrollmentChangeSchedulesOneReconcileForEnabledRelay() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE).edit()
            .clear()
            .putBoolean("relay-enabled.v1", true)
            .putString("relay-origin.v1", "https://community.example")
            .commit()
        RelayWakeRuntime.enabled = false
        RelayWakeRuntime.client = null

        RelayWakeRuntime.enrollmentChanged(context)

        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork("peppy-relay-wake-reconcile").get().size)
    }

    @Test fun failedCleanupKeepsPendingRegistrationWorkerRetryable() {
        assertTrue(relayWakeWorkerShouldRetry(RelayWakeReconcile.PENDING, cleanupRetry = true))
        assertFalse(relayWakeWorkerShouldRetry(RelayWakeReconcile.PENDING, cleanupRetry = false))
        assertTrue(relayWakeWorkerShouldRetry(RelayWakeReconcile.RETRY, cleanupRetry = false))
    }

    @Test fun workerRetriesWhenProtectedStateBecomesUnreadableAfterEntryValidation() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        val box = ReadabilityChangesAfterEntryBox()
        val cleanup = RelayWakeCleanup(
            RelayWakeIdentity(
                "https://account.example", "vault", "device", "fcm", "production", "fingerprint",
            ),
            RelayRoute("00000000-0000-4000-8000-000000000121", "manage", "wake"),
            "bearer",
            "https://relay.example",
        )
        assertTrue(RelayWakeStateStore(preferences, box).saveCleanup(cleanup))
        val storedCiphertext = preferences.getString("hosted-relay-cleanup.v2.${cleanup.route.routeId}", null)
        val previousFactory = relayWakeStateStoreFactory
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }

        try {
            val result = TestListenableWorkerBuilder<RelayWakeReconcileWorker>(context)
                .build()
                .startWork()
                .get(5, TimeUnit.SECONDS)

            assertEquals(ListenableWorker.Result.retry(), result)
            assertEquals(RelayWakeStatus.RETRYING, RelayWakeRuntime.status(context))
            assertEquals(2, box.openCalls)
            assertEquals(
                storedCiphertext,
                preferences.getString("hosted-relay-cleanup.v2.${cleanup.route.routeId}", null),
            )
            box.readableOpens = Int.MAX_VALUE
            assertEquals(cleanup.route.routeId, RelayWakeStateStore(preferences, box).cleanups().single().route.routeId)
        } finally {
            relayWakeStateStoreFactory = previousFactory
            preferences.edit().clear().commit()
        }
    }

    @Test fun legacyMismatchedCleanupNeverDeletesAccountAndRetriesRelayRevocationAfterReload() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        val box = PlaintextBox()
        val routeId = "00000000-0000-4000-8000-000000000122"
        val bearerA = "bearer-a"
        val bearerB = "bearer-b"
        val state = RelayWakeStateStore(preferences, box)
        val legacyCleanup = listOf(
            "https://account-a.example", "vault-a", "device-a", "fcm", "production", state.tokenFingerprint(bearerA),
            routeId, "manage-a", "wake-a", bearerB, "https://relay-a.example",
        ).joinToString("\n")
        val dispatch = RecordingCleanupDispatch(listOf(HttpResult(500, null), HttpResult(200, null)))
        val previousFactory = relayWakeStateStoreFactory
        preferences.edit().clear().putString("hosted-relay-cleanup.v2.$routeId", legacyCleanup).commit()
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }
        RecordingReconcileWorker.dispatch = dispatch

        try {
            assertEquals(ListenableWorker.Result.retry(), TestListenableWorkerBuilder<RecordingReconcileWorker>(context)
                .build().startWork().get(5, TimeUnit.SECONDS))
            assertTrue(dispatch.accountDeletes.isEmpty())
            assertEquals(listOf("https://relay-a.example" to "manage-a"), dispatch.revocations)
            assertEquals(routeId, RelayWakeStateStore(preferences, box).cleanups().single().route.routeId)

            assertEquals(ListenableWorker.Result.success(), TestListenableWorkerBuilder<RecordingReconcileWorker>(context)
                .build().startWork().get(5, TimeUnit.SECONDS))
            assertTrue(dispatch.accountDeletes.isEmpty())
            assertEquals(2, dispatch.revocations.size)
            assertTrue(RelayWakeStateStore(preferences, box).cleanups().isEmpty())
        } finally {
            RecordingReconcileWorker.dispatch = null
            relayWakeStateStoreFactory = previousFactory
            preferences.edit().clear().commit()
        }
    }

    @Test fun matchingLegacyCleanupDeletesCapturedAccountBeforeRevokingRelay() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
        val box = PlaintextBox()
        val routeId = "00000000-0000-4000-8000-000000000123"
        val bearerA = "bearer-a"
        val state = RelayWakeStateStore(preferences, box)
        val legacyCleanup = listOf(
            "https://account-a.example", "vault-a", "device-a", "fcm", "production", state.tokenFingerprint(bearerA),
            routeId, "manage-a", "wake-a", bearerA, "https://relay-a.example",
        ).joinToString("\n")
        val dispatch = RecordingCleanupDispatch(listOf(HttpResult(200, null)))
        val previousFactory = relayWakeStateStoreFactory
        preferences.edit().clear().putString("hosted-relay-cleanup.v2.$routeId", legacyCleanup).commit()
        relayWakeStateStoreFactory = { RelayWakeStateStore(it, box) }
        RecordingReconcileWorker.dispatch = dispatch

        try {
            assertEquals(ListenableWorker.Result.success(), TestListenableWorkerBuilder<RecordingReconcileWorker>(context)
                .build().startWork().get(5, TimeUnit.SECONDS))
            assertEquals(listOf(Triple("https://account-a.example", bearerA, routeId)), dispatch.accountDeletes)
            assertEquals(listOf("https://relay-a.example" to "manage-a"), dispatch.revocations)
            assertTrue(RelayWakeStateStore(preferences, box).cleanups().isEmpty())
        } finally {
            RecordingReconcileWorker.dispatch = null
            relayWakeStateStoreFactory = previousFactory
            preferences.edit().clear().commit()
        }
    }
}


private class ReadabilityChangesAfterEntryBox : SecretBox {
    var readableOpens = 1
    var openCalls = 0

    override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString()

    override fun open(purpose: String, sealed: String): ByteArray? {
        openCalls++
        return if (openCalls <= readableOpens) sealed.encodeToByteArray() else null
    }
}

private class PlaintextBox : SecretBox {
    override fun seal(purpose: String, plaintext: ByteArray) = plaintext.decodeToString()
    override fun open(purpose: String, sealed: String) = sealed.encodeToByteArray()
}

private class RecordingCleanupDispatch(private val revokeResults: List<HttpResult>) : RelayWakeWorkerDispatch {
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

class RecordingReconcileWorker(context: Context, params: WorkerParameters) :
    RelayWakeReconcileWorker(context, params, checkNotNull(dispatch)) {
    companion object {
        internal var dispatch: RelayWakeWorkerDispatch? = null
    }
}
