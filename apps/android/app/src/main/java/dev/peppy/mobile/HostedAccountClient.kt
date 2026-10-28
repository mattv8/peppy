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
import uniffi.peppy_mobile_bindings.NativeHostedClientKind
import uniffi.peppy_mobile_bindings.NativeHostedLoginAttempt
import uniffi.peppy_mobile_bindings.NativeHostedProvisioning
import uniffi.peppy_mobile_bindings.NativeHostedProvisioningView
import uniffi.peppy_mobile_bindings.hostedLoginRequest
import uniffi.peppy_mobile_bindings.hostedSessionRequest
import uniffi.peppy_mobile_bindings.parseHostedAccount
import uniffi.peppy_mobile_bindings.parseHostedLoginAttempt
import uniffi.peppy_mobile_bindings.parseHostedSession
import uniffi.peppy_mobile_bindings.parseHostedSessionRenewal
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
        try {
            if (key == SESSION_KEY) synchronized(sessionMutationLock) {
                synchronized(stateLock) { check(stamp); store.write(key, bytes) }
            } else synchronized(stateLock) {
                check(stamp)
                HostedProvisioningStorage.withStorageLock { store.write(key, bytes) }
            }
        } finally { bytes.fill(0) }
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
            resetRenewalGate()
            account(session, stamp)
        }
    }

    suspend fun account(): NativeHostedAccount = operation { account(session(), it) }

    /**
     * Extends the account session only after a foreground user interaction. Local expiry is
     * intentionally ignored here so the server can resolve clock skew or a lost prior response.
     */
    suspend fun renewAfterUserInteraction() = withContext(Dispatchers.IO) {
        val job = currentCoroutineContext()[Job]
        val now = nowMs()
        if (!beginRenewalAttempt(now) || !renewalLock.tryLock()) return@withContext
        try {
            val stamp = synchronized(stateLock) { Stamp(generation, job) }
            val observed = session(allowExpired = true)
            if (!renewalDue(observed, now)) return@withContext

            val response = transport.post(SESSION_RENEW_PATH, "{}", observed.token)
            check(stamp)
            if (response.code == 401) {
                removeObservedSession(stamp, observed)
                throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
            }
            val expiresInSeconds = parseHostedSessionRenewal(body(response), observed.accountId)
            saveRenewedSession(stamp, observed, now + expiresInSeconds.toLong() * 1_000)
        } finally {
            renewalLock.unlock()
        }
    }

    private fun session(allowExpired: Boolean = false): Session {
        val bytes = store.read(SESSION_KEY) ?: throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        try {
            val value = JSONObject(String(bytes, Charsets.UTF_8))
            val session = Session(value.getString("account_id"), value.getString("bearer"), value.getLong("expires_at"))
            if (!allowExpired && nowMs() >= session.expiresAt) throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
            return session
        } finally { bytes.fill(0) }
    }

    private fun renewalDue(session: Session, now: Long) =
        now >= session.expiresAt - (SESSION_LIFETIME_MS - RENEWAL_THROTTLE_MS)

    private fun beginRenewalAttempt(now: Long) = synchronized(renewalStateLock) {
        if (lastRenewalAttemptAtMs != Long.MIN_VALUE && now - lastRenewalAttemptAtMs < RENEWAL_ATTEMPT_BACKOFF_MS) false
        else { lastRenewalAttemptAtMs = now; true }
    }

    private fun resetRenewalGate() = synchronized(renewalStateLock) {
        lastRenewalAttemptAtMs = Long.MIN_VALUE
    }

    private fun currentSessionMatches(observed: Session): Boolean = try {
        session(allowExpired = true).let { it.accountId == observed.accountId && it.token == observed.token }
    } catch (_: Exception) {
        false
    }

    private fun saveRenewedSession(stamp: Stamp, observed: Session, expiresAt: Long) = synchronized(sessionMutationLock) {
        synchronized(stateLock) {
            check(stamp)
            if (!currentSessionMatches(observed)) throw kotlinx.coroutines.CancellationException()
            save(stamp, SESSION_KEY, JSONObject().put("account_id", observed.accountId).put("bearer", observed.token)
                .put("expires_at", expiresAt).toString().toByteArray())
        }
    }

    private fun removeObservedSession(stamp: Stamp, observed: Session) = synchronized(sessionMutationLock) {
        synchronized(stateLock) {
            check(stamp)
            if (currentSessionMatches(observed)) store.remove(SESSION_KEY)
        }
    }

    /** Native enrollment transport only; callers must never surface this account session. */
    internal fun enrollmentSessionHeader(): String? = try {
        session().token
    } catch (_: HostedFailure) {
        null
    }

    private fun account(session: Session, stamp: Stamp): NativeHostedAccount {
        check(stamp)
        if (nowMs() >= session.expiresAt) throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        val response = transport.get("/hosted/v1/account", session.token); check(stamp)
        if (response.code == 401) {
            removeObservedSession(stamp, session)
            resetRenewalGate()
            throw HostedFailure(HostedFailure.Kind.SESSION_EXPIRED)
        }
        return parseHostedAccount(body(response), session.accountId)
    }

    suspend fun signOut() = withContext(Dispatchers.IO) {
        val old = synchronized(sessionMutationLock) {
            synchronized(stateLock) {
                generation++
                val value = try { session(allowExpired = true) } catch (_: Exception) { null }
                store.remove(SESSION_KEY)
                value
            }
        }
        resetRenewalGate()
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
        val bytes = HostedProvisioningStorage.readCheckpoint(store) ?: return null
        try { return restoreHostedProvisioning(bytes, ORIGIN, session.accountId) }
        catch (_: Exception) { throw HostedFailure(HostedFailure.Kind.WRONG_ACCOUNT) }
        finally { bytes.fill(0) }
    }

    /**
     * The phrase an interrupted preparation already made durable for the signed-in account, so a restarted
     * screen offers it again instead of generating a conflicting one. Throws [HostedFailure.Kind.STORAGE]
     * when the secure store cannot be read.
     */
    suspend fun candidatePendingSecret(): String? = operation { stamp ->
        val session = session(); check(stamp)
        when (val pending = readPending(session)) {
            StoredSecret.Missing -> null
            is StoredSecret.Present -> try { String(pending.value) } finally { pending.value.fill('\u0000') }
        }
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
                persistPending(stamp, session, passphrase)
                return@operation view
            }
        }
        if (account.access != "read_write") throw HostedFailure(HostedFailure.Kind.ENTITLEMENT)
        if (account.vaultId != null) throw HostedFailure(HostedFailure.Kind.EXISTING_VAULT)
        prepareHostedProvisioning(
            ORIGIN,
            session.accountId,
            passphrase,
            NativeHostedClientKind.ANDROID,
            "Android phone",
        ).use { item ->
            persistPending(stamp, session, passphrase)
            save(stamp, CHECKPOINT_KEY, item.checkpoint())
            item.view()
        }
    }

    suspend fun completeVault(passphrase: String): ByteArray = operation { stamp ->
        val session = session()
        val item = pending(session) ?: throw HostedFailure(HostedFailure.Kind.INVALID_RESPONSE)
        item.use {
            if (!it.passphraseMatches(passphrase)) throw HostedFailure(HostedFailure.Kind.WRONG_PASSPHRASE)
            persistPending(stamp, session, passphrase)
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

    /**
     * Call only after the imported identity unlocked. Verifies the checkpoint's account, vault and device
     * before moving the pending secret to its binding, and removes the checkpoint only once the binding is
     * verified. Without a checkpoint nothing is verified, so nothing is bound.
     */
    suspend fun bindPendingSecretAndAcknowledge(vaultId: String, deviceId: String) = operation { stamp ->
        val session = session()
        val checkpoint = HostedProvisioningStorage.readCheckpoint(store) ?: return@operation
        try {
            val provisioning = try {
                restoreHostedProvisioning(checkpoint, ORIGIN, session.accountId)
            } catch (_: Exception) {
                throw HostedFailure(HostedFailure.Kind.WRONG_ACCOUNT)
            }
            provisioning.use {
                val view = it.view()
                if (view.vaultId != vaultId || view.deviceId != deviceId) invalid()
                val candidate = HostedProvisioningStorage.candidate(
                    store, checkpoint, ORIGIN, session.accountId, vaultId, deviceId,
                ) ?: throw HostedFailure(HostedFailure.Kind.STORAGE)
                try {
                    if (!it.passphraseMatches(candidate.concatToString())) {
                        throw HostedFailure(HostedFailure.Kind.STORAGE)
                    }
                    synchronized(stateLock) {
                        check(stamp)
                        if (!HostedProvisioningStorage.commitVerified(
                                store, checkpoint, ORIGIN, session.accountId, vaultId, deviceId, candidate,
                            )
                        ) {
                            throw HostedFailure(HostedFailure.Kind.STORAGE)
                        }
                    }
                } finally {
                    candidate.fill('\u0000')
                }
            }
        } finally {
            checkpoint.fill(0)
        }
    }

    private fun readPending(session: Session): StoredSecret = try {
        HostedProvisioningStorage.withStorageLock {
            VaultSecretStore(store).readPending(ORIGIN, session.accountId)
        }
    } catch (_: SecureStoreUnavailable) {
        throw HostedFailure(HostedFailure.Kind.STORAGE)
    }

    /** Fails closed when the account already holds a different phrase or the store cannot be read or written. */
    private fun persistPending(stamp: Stamp, session: Session, passphrase: String) {
        val secret = passphrase.toCharArray()
        try {
            synchronized(stateLock) {
                check(stamp)
                HostedProvisioningStorage.withStorageLock {
                    if (!VaultSecretStore(store).savePending(ORIGIN, session.accountId, secret)) {
                        throw HostedFailure(HostedFailure.Kind.STORAGE)
                    }
                }
            }
        } finally { secret.fill('\u0000') }
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
        private const val SESSION_RENEW_PATH = "/hosted/v1/auth/session/renew"
        private const val SESSION_LIFETIME_MS = 2_592_000_000L
        private const val RENEWAL_THROTTLE_MS = 15 * 60 * 1_000L
        private const val RENEWAL_ATTEMPT_BACKOFF_MS = 60 * 1_000L
        private val renewalLock = Mutex()
        private val renewalStateLock = Any()
        private var lastRenewalAttemptAtMs = Long.MIN_VALUE
        private val sessionMutationLock = Any()
    }
}
