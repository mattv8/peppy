package dev.peppy.mobile

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.SecureRandom
import java.util.UUID
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.google.android.gms.tasks.Tasks
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.hostedWakeRelayUrl
import uniffi.peppy_mobile_bindings.parsePushConfig

/** Content-free relay enrollment. It is deliberately inert until the host enables it. */
/** Secrets deliberately have no generated toString/copy/component APIs. */
class RelayRoute(val routeId: String, val manageCredential: String, val wakeCredential: String) {
    override fun toString() = "RelayRoute(redacted)"
}

interface RelayWakeTransport {
    @Throws(IOException::class) fun post(path: String, body: String): HttpResult
    @Throws(IOException::class) fun get(path: String): HttpResult = throw IOException("GET unsupported")
}

/** The relay's unauthenticated capability document, never the server push-config DTO, owns this gate. */
internal fun relaySupportsCandidates(transport: RelayWakeTransport): Boolean? {
    val response = transport.get("/v1/capabilities")
    if (!response.ok) return null
    return try { JSONObject(requireNotNull(response.body)).getBoolean("confirm_candidates") } catch (_: Exception) { null }
}

fun interface RelayRoutePublisher { @Throws(IOException::class) fun publish(routeId: String, wakeCredential: String) }
interface RelayRouteStore {
    fun route(): RelayRoute?
    fun save(route: RelayRoute)
    fun clear()
    fun pendingRegistration(): String?
    fun savePendingRegistration(id: String)
    fun clearPendingRegistration()
}

/** Keystore-wrapped route credentials. Provider tokens and manage credentials never leave this store. */
class RelayCredentialStore(private val preferences: SharedPreferences, private val secrets: SecretBox = KeystoreSecretBox) : RelayRouteStore {
    override fun route(): RelayRoute? = preferences.getString(KEY, null)?.let { sealed ->
        secrets.open(PURPOSE, sealed)?.let { bytes ->
            try {
                val value = JSONObject(String(bytes, Charsets.UTF_8))
                RelayRoute(value.getString("route_id"), value.getString("manage_credential"), value.getString("wake_credential"))
            } catch (_: Exception) { null }
        }
    }

    override fun save(route: RelayRoute) {
        val body = JSONObject().put("route_id", route.routeId).put("manage_credential", route.manageCredential)
            .put("wake_credential", route.wakeCredential).toString().toByteArray(Charsets.UTF_8)
        preferences.edit().putString(KEY, secrets.seal(PURPOSE, body)).apply()
    }

    override fun clear() { preferences.edit().remove(KEY).apply() }
    override fun pendingRegistration(): String? = preferences.getString(PENDING_KEY, null)?.let { secrets.open(PENDING_PURPOSE, it) }
        ?.toString(Charsets.UTF_8)?.takeIf { UUID_RE.matches(it) }
    override fun savePendingRegistration(id: String) { preferences.edit().putString(PENDING_KEY, secrets.seal(PENDING_PURPOSE, id.toByteArray())).apply() }
    override fun clearPendingRegistration() { preferences.edit().remove(PENDING_KEY).apply() }
    private companion object {
        const val KEY = "relay-route-v1"; const val PURPOSE = "relay-route-v1"
        const val PENDING_KEY = "relay-pending-v1"; const val PENDING_PURPOSE = "relay-pending-v1" // gitleaks:allow -- preference key and encryption-purpose labels, not credentials
        val UUID_RE = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", RegexOption.IGNORE_CASE)
    }
}

class RelayWakeClient(
    private val transport: RelayWakeTransport,
    private val credentials: RelayRouteStore,
    private val publisher: RelayRoutePublisher,
    private val provider: String = "fcm",
    private val supportsCandidates: Boolean = false,
) {
    /** Starts the push challenge. The caller must pass its provider token; it is never persisted here. */
    fun register(deviceToken: String): String? {
        if (!validToken(deviceToken)) return null
        val response = transport.post("/v1/registrations", JSONObject().put("provider", provider).put("device_token", deviceToken).toString())
        if (response.code != 201) return null
        return try { JSONObject(requireNotNull(response.body)).getString("registration_id").takeIf { UUID_RE.matches(it) }?.also { credentials.savePendingRegistration(it) } } catch (_: Exception) { null }
    }

    /** Called only after the provider delivered the opaque challenge. */
    fun confirm(challenge: String, installationPublicIdentity: String? = null): Boolean {
        val registrationId = credentials.pendingRegistration() ?: return false
        if (!UUID_RE.matches(registrationId) || !SECRET_RE.matches(challenge)) return false
        val candidate = if (supportsCandidates) RelayRoute(UUID.randomUUID().toString(), randomSecret(), randomSecret()) else null
        val body = JSONObject().put("challenge", challenge).also {
            if (candidate != null) it.put("route_id", candidate.routeId).put("manage_credential", candidate.manageCredential).put("wake_credential", candidate.wakeCredential)
            if (installationPublicIdentity != null) it.put("installation_public_identity", installationPublicIdentity)
        }
        val response = transport.post("/v1/registrations/$registrationId/confirm", body.toString())
        if (response.code != 201) return false
        val route = try {
            val value = JSONObject(requireNotNull(response.body))
            RelayRoute(value.getString("route_id"), value.getString("manage_credential"), value.getString("wake_credential"))
        } catch (_: Exception) { return false }
        if (candidate != null && (route.routeId != candidate.routeId || route.manageCredential != candidate.manageCredential || route.wakeCredential != candidate.wakeCredential)) return false
        credentials.save(route)
        try { publisher.publish(route.routeId, route.wakeCredential) } catch (_: IOException) { return false }
        credentials.clearPendingRegistration()
        return true
    }

    private fun validToken(token: String) = token.isNotBlank() && token.toByteArray(Charsets.UTF_8).size <= 4096
    private companion object {
        val UUID_RE = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", RegexOption.IGNORE_CASE)
        val SECRET_RE = Regex("[0-9a-f]{64}", RegexOption.IGNORE_CASE)
        fun randomSecret(): String = ByteArray(32).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it) }
    }
}

/** Fixed-origin, redirect-refusing relay transport; relay endpoints are unauthenticated by design. */
class AndroidRelayWakeTransport(origin: String) : RelayWakeTransport {
    private val origin = requireNotNull(CredentialParser.canonicalOrigin(origin, false))
    override fun post(path: String, body: String): HttpResult {
        require(path.startsWith("/v1/") && !path.contains('?') && !path.contains('#') && !path.contains("//"))
        val connection = URL(origin + path).openConnection() as HttpURLConnection
        try {
            val bytes = body.toByteArray(Charsets.UTF_8)
            connection.instanceFollowRedirects = false; connection.useCaches = false; connection.requestMethod = "POST"
            connection.connectTimeout = GatewayHttp.TIMEOUT_MS; connection.readTimeout = GatewayHttp.TIMEOUT_MS
            connection.setRequestProperty("Content-Type", "application/json"); connection.setRequestProperty("Accept", "application/json")
            connection.doOutput = true; connection.setFixedLengthStreamingMode(bytes.size)
            connection.outputStream.use { it.write(bytes) }
            val code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            return HttpResult(code, stream?.use { GatewayHttp.readBounded(it, GatewayHttp.MAX_ERROR_BYTES) })
        } finally { connection.disconnect() }
    }
    override fun get(path: String): HttpResult {
        require(path == "/v1/capabilities")
        val connection = URL(origin + path).openConnection() as HttpURLConnection
        try {
            connection.instanceFollowRedirects = false; connection.useCaches = false; connection.requestMethod = "GET"
            connection.connectTimeout = GatewayHttp.TIMEOUT_MS; connection.readTimeout = GatewayHttp.TIMEOUT_MS
            connection.setRequestProperty("Accept", "application/json")
            val code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            return HttpResult(code, stream?.use { GatewayHttp.readBounded(it, GatewayHttp.MAX_ERROR_BYTES) })
        } finally { connection.disconnect() }
    }
}

/** Host-owned injection point; unset means relay enrollment and wakes are disabled. */
enum class RelayWakePreference { AUTOMATIC, ENABLED, PAUSED }
enum class RelayWakeStatus { PENDING, REGISTERED, RETRYING, PAUSED, UNAVAILABLE, MISSING_APP_SUPPORT, CLEANUP_PENDING }

internal class PausedOutcome(val status: RelayWakeStatus, val retry: Boolean)

/** Durable scheduling and preference boundary. All network work runs in [RelayWakeReconcileWorker]. */
object RelayWakeRuntime {
    @Volatile var enabled: Boolean = false
    @Volatile var client: RelayWakeClient? = null

    private const val PREFS = "peppy-relay"
    private const val ENABLED = "relay-enabled.v1"
    private const val ORIGIN = "relay-origin.v1"
    private const val PREFERENCE = "hosted-relay-preference.v1"
    private const val STATUS = "hosted-relay-status.v1"

    /** Rebuild volatile transport state after process death; secrets stay in [RelayCredentialStore]. */
    fun initialize(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val origin = prefs.getString(ORIGIN, null)?.let { CredentialParser.canonicalOrigin(it, false) }
        val credential = NativeGateway.accountCredential(context)
        // Startup retires an upgraded v1 custom route using the persisted relay origin, so existing
        // users who never touch the toggle still queue cleanup before any v2 route is published.
        if (!migrateLegacyCustomRoute(prefs, credential, origin)) { enabled = false; client = null; return }
        // Queued cleanup must run even while paused or disabled.
        if (RelayWakeStateStore(prefs).cleanups().isNotEmpty()) schedule(context)
        val hostedRelay = credential?.let { hostedWakeRelayUrl(it.origin) }
        val preference = preference(context)
        if (hostedRelay != null) {
            if (preference == RelayWakePreference.PAUSED) {
                enabled = false
                client = null
                return
            }
            client = RelayWakeClient(AndroidRelayWakeTransport(hostedRelay), RelayCredentialStore(prefs), RelayRoutePublisher { routeId, wakeCredential -> GatewayAccountHost.publishWake(context, routeId, wakeCredential) })
            enabled = true
            schedule(context)
            return
        }
        if (!prefs.getBoolean(ENABLED, false) || origin == null) { enabled = false; client = null; return }
        client = RelayWakeClient(AndroidRelayWakeTransport(origin), RelayCredentialStore(prefs),
            RelayRoutePublisher { routeId, wakeCredential -> GatewayAccountHost.publishWake(context, routeId, wakeCredential) })
        enabled = true
        schedule(context)
    }

    /** Retire an old custom route only when its explicit saved relay origin is still available. */
    internal fun migrateLegacyCustomRoute(
        prefs: SharedPreferences,
        credential: GatewayAccountCredential?,
        relayOrigin: String?,
        routeStore: RelayRouteStore = RelayCredentialStore(prefs),
        state: RelayWakeStateStore = RelayWakeStateStore(prefs),
    ): Boolean {
        val route = routeStore.route() ?: return true
        val account = credential ?: return false
        val origin = relayOrigin ?: return false
        val identity = RelayWakeIdentity(account.origin, account.vaultId, account.deviceId, "fcm", "production", state.tokenFingerprint(account.bearerToken))
        if (!state.saveCleanup(RelayWakeCleanup(identity, route, account.bearerToken, origin))) return false
        routeStore.clear()
        return true
    }

    fun setEnabled(context: Context, origin: String?, value: Boolean) {
        val canonical = origin?.let { CredentialParser.canonicalOrigin(it, false) }
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val previousOrigin = prefs.getString(ORIGIN, null)?.let { CredentialParser.canonicalOrigin(it, false) }
        if (!migrateLegacyCustomRoute(prefs, NativeGateway.accountCredential(context), previousOrigin)) return
        val editor = prefs.edit().putBoolean(ENABLED, value && canonical != null)
        if (canonical != null) editor.putString(ORIGIN, canonical)
        editor.apply()
        initialize(context)
        if (!value) schedule(context)
    }

    fun preference(context: Context): RelayWakePreference {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        return when (prefs.getString(PREFERENCE, null)) {
            "enabled" -> RelayWakePreference.ENABLED
            "paused" -> RelayWakePreference.PAUSED
            "automatic" -> RelayWakePreference.AUTOMATIC
            null -> {
                val migrated = when {
                    !prefs.contains(ENABLED) -> RelayWakePreference.AUTOMATIC
                    prefs.getBoolean(ENABLED, false) -> RelayWakePreference.ENABLED
                    else -> RelayWakePreference.PAUSED
                }
                if (prefs.edit().putString(PREFERENCE, migrated.name.lowercase()).commit()) migrated else RelayWakePreference.PAUSED
            }
            else -> RelayWakePreference.PAUSED
        }
    }

    fun setPreference(context: Context, preference: RelayWakePreference) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (!prefs.edit().putString(PREFERENCE, preference.name.lowercase()).commit()) return
        if (preference == RelayWakePreference.PAUSED) {
            val state = RelayWakeStateStore(prefs)
            val credential = NativeGateway.accountCredential(context)
            val active = state.active
            if (credential != null && active != null) {
                val identity = RelayWakeIdentity(credential.origin, credential.vaultId, credential.deviceId, "fcm", "production", state.tokenFingerprint(credential.bearerToken))
                if (active.identity.matches(identity) && state.saveCleanup(RelayWakeCleanup(active.identity, active.route, active.bearerToken, active.relayOrigin))) state.active = null
            }
            setStatus(context, if (state.cleanups().isEmpty()) RelayWakeStatus.PAUSED else RelayWakeStatus.CLEANUP_PENDING)
        }
        initialize(context)
        // Cleanup must run while paused; initialize correctly disables wake acceptance.
        schedule(context)
    }

    /**
     * Wake acceptance is off: retire any committed route, then decide the status and retry from what
     * is still durable. Success is allowed only when no route or cleanup remains.
     */
    internal fun retirePausedWork(state: RelayWakeStateStore, cleanupFailed: Boolean): PausedOutcome {
        state.active?.let { active ->
            if (state.saveCleanup(RelayWakeCleanup(active.identity, active.route, active.bearerToken, active.relayOrigin))) state.active = null
        }
        val cleanupPending = cleanupFailed || state.active != null || state.cleanups().isNotEmpty()
        return PausedOutcome(if (cleanupPending) RelayWakeStatus.CLEANUP_PENDING else RelayWakeStatus.PAUSED, cleanupPending)
    }

    fun status(context: Context): RelayWakeStatus = runCatching {
        RelayWakeStatus.valueOf(context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(STATUS, RelayWakeStatus.PENDING.name)!!)
    }.getOrDefault(RelayWakeStatus.PENDING)

    fun acceptsWakes(context: Context): Boolean {
        val credential = NativeGateway.accountCredential(context)
        return if (credential?.let { hostedWakeRelayUrl(it.origin) } != null) preference(context) != RelayWakePreference.PAUSED && enabled else enabled
    }
    fun tokenChanged(context: Context) { if (acceptsWakes(context)) schedule(context) }
    /** Firebase callbacks only persist/schedule; relay HTTP is serialized in the worker. */
    fun challenge(context: Context, value: String) {
        if (!acceptsWakes(context) || !Regex("[0-9a-f]{64}", RegexOption.IGNORE_CASE).matches(value)) return
        RelayWakeStateStore(context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)).saveEarlyChallenge(value)
        schedule(context)
    }
    /** Enrollment completion and foreground restoration use the same serialized reconciliation path. */
    fun enrollmentChanged(context: Context) { initialize(context); if (enabled) schedule(context) }
    /** Archive removes the binding but never changes the user's durable wake preference. */
    fun archiveEnrollment(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val state = RelayWakeStateStore(prefs)
        val credential = NativeGateway.accountCredential(context)
        val active = state.active
        if (credential != null && active != null) {
            val identity = RelayWakeIdentity(credential.origin, credential.vaultId, credential.deviceId, "fcm", "production", state.tokenFingerprint(credential.bearerToken))
            if (active.identity.matches(identity) && state.saveCleanup(RelayWakeCleanup(active.identity, active.route, active.bearerToken, active.relayOrigin))) {
                state.active = null
            }
        }
        state.pending?.let { pending ->
            if (credential != null && pending.registrationId != null && pending.challenge != null) {
                state.saveCleanup(RelayWakeCleanup(pending.identity, pending.candidate, credential.bearerToken, pending.relayOrigin))
            }
        }
        state.pending = null
        enabled = false
        client = null
        setStatus(context, if (state.cleanups().isEmpty()) RelayWakeStatus.PAUSED else RelayWakeStatus.CLEANUP_PENDING)
        schedule(context)
    }

    fun schedule(context: Context) {
        WorkManager.getInstance(context).enqueueUniqueWork(
            "peppy-relay-wake-reconcile", ExistingWorkPolicy.APPEND_OR_REPLACE,
            OneTimeWorkRequestBuilder<RelayWakeReconcileWorker>()
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, java.util.concurrent.TimeUnit.SECONDS)
                .build(),
        )
    }

    internal fun setStatus(context: Context, status: RelayWakeStatus) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(STATUS, status.name).apply()
    }
}

class RelayWakeReconcileWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        val state = RelayWakeStateStore(applicationContext.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE))
        var cleanupRetry = false
        state.cleanups().forEach { cleanup ->
            try {
                val deleted = GatewayAccountHost.deleteWakeRoute(cleanup.identity.origin, cleanup.bearerToken, cleanup.route.routeId)
                if (!deleted.ok && deleted.code !in setOf(401, 403, 404, 405)) { cleanupRetry = true; return@forEach }
                val revoked = AndroidRelayWakeTransport(cleanup.relayOrigin).post("/v1/routes/${cleanup.route.routeId}/revoke", JSONObject().put("manage_credential", cleanup.route.manageCredential).toString())
                if (!revoked.ok && revoked.code !in setOf(401, 403)) { cleanupRetry = true; return@forEach }
                if (!state.clearCleanup(cleanup.route.routeId)) cleanupRetry = true
            } catch (_: IOException) { cleanupRetry = true }
        }
        if (!RelayWakeRuntime.acceptsWakes(applicationContext)) {
            // Reconcile can race a foreground pause after confirm. Retire whatever committed; a queued
            // cleanup must be retried by WorkManager while paused, never reported as plain PAUSED.
            val outcome = RelayWakeRuntime.retirePausedWork(state, cleanupRetry)
            RelayWakeRuntime.setStatus(applicationContext, outcome.status)
            return@withContext if (outcome.retry) Result.retry() else Result.success()
        }
        val credential = NativeGateway.accountCredential(applicationContext) ?: return@withContext Result.success()
        val hostedRelay = hostedWakeRelayUrl(credential.origin)
        val customRelay = applicationContext.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE)
            .getString("relay-origin.v1", null)?.takeIf { applicationContext.getSharedPreferences("peppy-relay", Context.MODE_PRIVATE).getBoolean("relay-enabled.v1", false) }
        val relay = hostedRelay ?: customRelay ?: return@withContext Result.success()
        val config = if (hostedRelay == null) null else try { GatewayAccountHost.pushConfig(applicationContext) } catch (_: IOException) {
            RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.RETRYING); return@withContext Result.retry()
        }
        if (config != null) {
            if (config.code == 404) { RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.UNAVAILABLE); return@withContext Result.success() }
            if (!config.ok) { RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.RETRYING); return@withContext Result.retry() }
            val parsed = try { parsePushConfig(requireNotNull(config.body)) } catch (_: Exception) {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.UNAVAILABLE); return@withContext Result.success()
            }
            if (!parsed.available || "fcm" !in parsed.providers) { RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.UNAVAILABLE); return@withContext Result.success() }
        }
        if (applicationContext.resources.getIdentifier("google_app_id", "string", applicationContext.packageName) == 0) {
            RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.MISSING_APP_SUPPORT); return@withContext Result.success()
        }
        val token = try { Tasks.await(FirebaseMessaging.getInstance().token) } catch (_: Exception) {
            RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.RETRYING); return@withContext Result.retry()
        }
        val fingerprint = state.tokenFingerprint(token)
        val identity = RelayWakeIdentity(credential.origin, credential.vaultId, credential.deviceId, "fcm", "production", state.tokenFingerprint(credential.bearerToken))
        val coordinator = RelayWakeCoordinator(
            state,
            AndroidRelayWakeTransport(relay),
            RelayRoutePublisher { routeId, wakeCredential ->
                // Never publish a saved old route using a credential installed during the request.
                val current = NativeGateway.accountCredential(applicationContext)
                if (current?.let { it.origin == credential.origin && it.vaultId == credential.vaultId && it.deviceId == credential.deviceId && it.bearerToken == credential.bearerToken } != true) {
                    throw IOException("relay identity changed")
                }
                GatewayAccountHost.publishWake(credential.origin, credential.bearerToken, routeId, wakeCredential)
            },
            retire = { old ->
                state.saveCleanup(RelayWakeCleanup(old.identity, old.route, old.bearerToken, old.relayOrigin))
            },
            retirePending = { pending ->
                state.saveCleanup(RelayWakeCleanup(pending.identity, pending.candidate, credential.bearerToken, pending.relayOrigin))
            },
            publishActive = { active ->
                val current = NativeGateway.accountCredential(applicationContext)
                if (current?.let { it.origin == active.identity.origin && it.vaultId == active.identity.vaultId && it.deviceId == active.identity.deviceId && state.tokenFingerprint(it.bearerToken) == active.identity.credentialFingerprint } != true) {
                    throw IOException("relay identity changed")
                }
                GatewayAccountHost.publishWake(active.identity.origin, active.bearerToken, active.route.routeId, active.route.wakeCredential)
            },
        )
        state.earlyChallenge()?.let { early ->
            if (state.pending?.registrationId != null) {
                coordinator.saveChallenge(early)
                state.clearEarlyChallenge()
            }
        }
        // Capability is a relay concern. The account push-config DTO intentionally contains no
        // relay implementation details, so never infer this from the authenticated server reply.
        val supportsCandidates = if (hostedRelay != null) {
            val supported = try { relaySupportsCandidates(AndroidRelayWakeTransport(relay)) } catch (_: IOException) {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.RETRYING); return@withContext Result.retry()
            }
            if (supported != true) {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.UNAVAILABLE); return@withContext Result.success()
            }
            supported
        } else false
        var result = coordinator.reconcile(identity, relay, if (hostedRelay == null) "community" else "hosted", token, fingerprint, credential.bearerToken, supportsCandidates)
        // A callback can arrive between register completion and this line. It only becomes proof
        // after the registration id is committed, then the same coordinator consumes it.
        state.earlyChallenge()?.let { early ->
            if (state.pending?.registrationId != null) {
                coordinator.saveChallenge(early)
            }
            if (state.pending?.registrationId != null && state.clearEarlyChallenge()) {
                result = coordinator.reconcile(identity, relay, if (hostedRelay == null) "community" else "hosted", token, fingerprint, credential.bearerToken, supportsCandidates)
            }
        }
        if (!RelayWakeRuntime.acceptsWakes(applicationContext)) {
            val outcome = RelayWakeRuntime.retirePausedWork(state, cleanupFailed = false)
            RelayWakeRuntime.setStatus(applicationContext, outcome.status)
            return@withContext if (outcome.retry) Result.retry() else Result.success()
        }
        when (result) {
            RelayWakeReconcile.REGISTERED -> {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.REGISTERED)
                if (cleanupRetry) Result.retry() else Result.success()
            }
            RelayWakeReconcile.PENDING -> {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.PENDING)
                Result.success()
            }
            RelayWakeReconcile.RETRY -> {
                RelayWakeRuntime.setStatus(applicationContext, RelayWakeStatus.RETRYING)
                Result.retry()
            }
        }
    }
}
