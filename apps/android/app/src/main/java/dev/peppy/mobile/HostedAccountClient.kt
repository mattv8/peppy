package dev.peppy.mobile

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeHostedAccount
import uniffi.peppy_mobile_bindings.NativeHostedLoginAttempt
import uniffi.peppy_mobile_bindings.NativeHostedProvisioning
import uniffi.peppy_mobile_bindings.NativeHostedProvisioningView
import uniffi.peppy_mobile_bindings.hostedLoginRequest
import uniffi.peppy_mobile_bindings.hostedSessionRequest
import uniffi.peppy_mobile_bindings.parseHostedAccount
import uniffi.peppy_mobile_bindings.parseHostedLoginAttempt
import uniffi.peppy_mobile_bindings.parseHostedSession
import uniffi.peppy_mobile_bindings.prepareHostedProvisioning
import uniffi.peppy_mobile_bindings.restoreHostedProvisioning

internal interface HostedTransport {
    fun get(path: String, bearer: String? = null): HttpResult
    fun post(path: String, body: String, bearer: String? = null): HttpResult
    fun delete(path: String, bearer: String): HttpResult
}

internal object ProductionHostedTransport : HostedTransport {
    override fun get(path: String, bearer: String?) = HostedHttp(bearer).get(path)
    override fun post(path: String, body: String, bearer: String?) = HostedHttp(bearer).postJson(path, body)
    override fun delete(path: String, bearer: String) = HostedHttp(bearer).delete(path)
}

internal interface HostedSecureStore {
    fun read(key: String): ByteArray?
    fun write(key: String, value: ByteArray)
    fun remove(key: String)
}

internal class AndroidHostedSecureStore(context: Context) : HostedSecureStore {
    private val preferences = context.getSharedPreferences("peppy-hosted-secure", Context.MODE_PRIVATE)
    override fun read(key: String): ByteArray? {
        val sealed = preferences.getString(key, null) ?: return null
        return KeystoreSecretBox.open(key, sealed) ?: throw HostedFailure(HostedFailure.Kind.STORAGE)
    }
    override fun write(key: String, value: ByteArray) {
        if (!preferences.edit().putString(key, KeystoreSecretBox.seal(key, value)).commit()) {
            throw HostedFailure(HostedFailure.Kind.STORAGE)
        }
    }
    override fun remove(key: String) {
        if (!preferences.edit().remove(key).commit()) throw HostedFailure(HostedFailure.Kind.STORAGE)
    }
}

internal class HostedFailure(val kind: Kind) : Exception(kind.name) {
    enum class Kind { UNAVAILABLE, SESSION_EXPIRED, WRONG_ACCOUNT, WRONG_PASSPHRASE, INVALID_RESPONSE, STORAGE, ENTITLEMENT, EXISTING_VAULT }
}

/** All HTTP and key derivation run off the UI thread. No mock implementation is a product default. */
internal class HostedAccountClient(
    private val store: HostedSecureStore,
    private val transport: HostedTransport = ProductionHostedTransport,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private class Session(val accountId: String, val token: String, val expiresAt: Long) {
        override fun toString() = "HostedSession(accountId=$accountId, token=[redacted])"
    }
    private class Stamp(val generation: Long, val job: Job?)
    private val operationLock = Mutex()
    private val stateLock = Any()
    private var generation = 0L

    private suspend fun <T> operation(body: suspend (Stamp) -> T): T = withContext(Dispatchers.IO) {
        operationLock.withLock {
            val job = currentCoroutineContext()[Job]
            val stamp = synchronized(stateLock) { Stamp(generation, job) }
            body(stamp)
        }
    }
    private fun check(stamp: Stamp) = synchronized(stateLock) {
        stamp.job?.ensureActive()
        if (generation != stamp.generation) throw kotlinx.coroutines.CancellationException()
    }
    private fun save(stamp: Stamp, key: String, bytes: ByteArray) {
        try { synchronized(stateLock) { check(stamp); store.write(key, bytes) } }
        finally { bytes.fill(0) }
    }
    fun cancelPendingWork() { synchronized(stateLock) { generation++ } }

    suspend fun availableProviders(): Set<String> = operation { stamp ->
        val response = transport.get("/hosted/v1/auth/config"); check(stamp)
        val json = JSONObject(body(response))
        if (json.keys().asSequence().toSet() != setOf("available_providers")) invalid()
        val array = json.getJSONArray("available_providers")
        val providers = (0 until array.length()).map { array.getString(it) }
        if (providers.any { it !in setOf("google", "apple") } || providers.toSet().size != providers.size) invalid()
        providers.toSet()
    }

    suspend fun beginGoogleSignIn(): NativeHostedLoginAttempt = operation { stamp ->
        val response = transport.post("/hosted/v1/auth/attempts", hostedLoginRequest("google")); check(stamp)
        parseHostedLoginAttempt(body(response))
    }

    suspend fun finishGoogleSignIn(attemptId: String, idToken: String): NativeHostedAccount = operation { stamp ->
        val response = transport.post("/hosted/v1/auth/session", hostedSessionRequest(attemptId, idToken)); check(stamp)
        parseHostedSession(body(response)).use { parsed ->
            val session = Session(parsed.accountId(), parsed.bearerToken(), nowMs() + parsed.expiresInSeconds().toLong() * 1_000)
            save(stamp, SESSION_KEY, JSONObject().put("account_id", session.accountId).put("bearer", session.token)
                .put("expires_at", session.expiresAt).toString().toByteArray())
            account(session, stamp)
        }
    }

    suspend fun account(): NativeHostedAccount = operation { account(session(), it) }

    private fun session(allowExpired: Boolean = false): Session {
        val bytes = store.read(SESSION_KEY) ?: throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        try {
            val value = JSONObject(String(bytes, Charsets.UTF_8))
            val session = Session(value.getString("account_id"), value.getString("bearer"), value.getLong("expires_at"))
            if (!allowExpired && nowMs() >= session.expiresAt) throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
            return session
        } finally { bytes.fill(0) }
    }

    private fun account(session: Session, stamp: Stamp): NativeHostedAccount {
        check(stamp)
        if (nowMs() >= session.expiresAt) throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        val response = transport.get("/hosted/v1/account", session.token); check(stamp)
        if (response.code == 401) {
            synchronized(stateLock) { check(stamp); store.remove(SESSION_KEY) }
            throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        }
        return parseHostedAccount(body(response), session.accountId)
    }

    suspend fun signOut() = withContext(Dispatchers.IO) {
        val old = synchronized(stateLock) {
            generation++
            val value = try { session(allowExpired = true) } catch (_: Exception) { null }
            store.remove(SESSION_KEY)
            value
        }
        if (old != null) {
            try { transport.delete("/hosted/v1/auth/session", old.token) }
            catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
            catch (_: Exception) { /* Local sign-out is complete; server session expires independently. */ }
        }
    }

    suspend fun pendingProvisioning(): NativeHostedProvisioning? = operation { stamp ->
        check(stamp); pending(session())
    }

    private fun pending(session: Session): NativeHostedProvisioning? {
        val bytes = store.read(CHECKPOINT_KEY) ?: return null
        try { return restoreHostedProvisioning(bytes, ORIGIN, session.accountId) }
        catch (_: Exception) { throw HostedFailure(HostedFailure.Kind.WRONG_ACCOUNT) }
        finally { bytes.fill(0) }
    }

    suspend fun prepareVault(passphrase: String): NativeHostedProvisioningView = operation { stamp ->
        val session = session()
        val account = account(session, stamp)
        val existing = pending(session)
        if (existing != null) {
            existing.use {
                if (!it.passphraseMatches(passphrase)) throw HostedFailure(HostedFailure.Kind.WRONG_PASSPHRASE)
                val view = it.view()
                if (account.vaultId != null && account.vaultId != view.vaultId) throw HostedFailure(HostedFailure.Kind.EXISTING_VAULT)
                return@operation view
            }
        }
        if (account.access != "read_write") throw HostedFailure(HostedFailure.Kind.ENTITLEMENT)
        if (account.vaultId != null) throw HostedFailure(HostedFailure.Kind.EXISTING_VAULT)
        prepareHostedProvisioning(ORIGIN, session.accountId, passphrase).use { item ->
            save(stamp, CHECKPOINT_KEY, item.checkpoint())
            item.view()
        }
    }

    suspend fun completeVault(passphrase: String): ByteArray = operation { stamp ->
        val session = session()
        val item = pending(session) ?: throw HostedFailure(HostedFailure.Kind.INVALID_RESPONSE)
        item.use {
            if (!it.passphraseMatches(passphrase)) throw HostedFailure(HostedFailure.Kind.WRONG_PASSPHRASE)
            fun grant() {
                check(stamp)
                val response = transport.post("/hosted/v1/provisioning", it.grantRequest(), session.token); check(stamp)
                it.acceptGrant(body(response)); save(stamp, CHECKPOINT_KEY, it.checkpoint())
            }
            if (!it.hasGrant()) grant()
            check(stamp)
            var response = transport.post("/hosted/v1/provisioning/complete", it.completeRequest(), session.token); check(stamp)
            if (response.code == 401) {
                val account = account(session, stamp)
                if (account.vaultId == null && account.access == "read_write") {
                    grant()
                    response = transport.post("/hosted/v1/provisioning/complete", it.completeRequest(), session.token); check(stamp)
                }
            }
            val credential = it.credentialJson(body(response)).toByteArray()
            check(stamp)
            credential
        }
    }

    suspend fun acknowledgeEnrollment(vaultId: String, deviceId: String) = operation { stamp ->
        pending(session())?.use {
            val view = it.view()
            if (view.vaultId != vaultId || view.deviceId != deviceId) invalid()
            synchronized(stateLock) { check(stamp); store.remove(CHECKPOINT_KEY) }
        }
    }

    private fun body(response: HttpResult): String {
        if (response.code == 401) throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        if (response.code == 403) throw HostedFailure(HostedFailure.Kind.ENTITLEMENT)
        if (response.code !in setOf(200, 201) || response.body == null) throw HostedFailure(HostedFailure.Kind.UNAVAILABLE)
        if (response.body.toByteArray().size > 64 * 1024) invalid()
        return response.body
    }
    private fun invalid(): Nothing = throw HostedFailure(HostedFailure.Kind.INVALID_RESPONSE)

    companion object {
        const val ORIGIN = "https://peppy.pro"
        const val SESSION_KEY = "hosted-session.v2"
        const val CHECKPOINT_KEY = "hosted-provisioning.v1"
    }
}
