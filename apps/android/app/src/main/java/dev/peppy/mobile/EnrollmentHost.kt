package dev.peppy.mobile

import android.content.Context
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeEnrollmentClient
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.NativeEnrollmentIdentity
import uniffi.peppy_mobile_bindings.newNativeEnrollmentClient
import uniffi.peppy_mobile_bindings.parseVaultInviteLink as parseNativeVaultInviteLink
import uniffi.peppy_mobile_bindings.restoreNativeEnrollmentClient
import java.net.HttpURLConnection
import java.net.URL
import java.util.Base64
import java.security.MessageDigest

internal data class EnrollmentHttpResponse(val status: Int, val body: String?)

internal fun interface EnrollmentTransport {
    fun request(origin: String, method: String, path: String, body: String?, headers: JSONObject?): EnrollmentHttpResponse?
}

internal data class InstanceDiscovery(val claimed: Boolean, val setupCodeRequired: Boolean)

internal fun parseInstanceDiscovery(discovery: String): InstanceDiscovery? = try {
    val instance = JSONObject(discovery)
    val claimed = instance.get("claimed") as? Boolean ?: return null
    val setupCodeRequired = (instance.opt("setup_required") as? Boolean) ?: true
    InstanceDiscovery(claimed, setupCodeRequired)
} catch (_: Exception) {
    null
}

internal fun claimErrorResult(response: String?): ImportResult? = try {
    if (response == null || response.length > MAX_CLAIM_ERROR_BYTES) null else when (JSONObject(response).opt("code")) {
        "invalid_setup_token" -> ImportResult.SETUP_TOKEN_REJECTED
        "instance_already_claimed" -> ImportResult.INSTANCE_ALREADY_CLAIMED
        else -> null
    }
} catch (_: Exception) {
    null
}

private const val MAX_CLAIM_ERROR_BYTES = 8 * 1024

/** Native enrollment transport. Secret proofs stay inside [NativeEnrollmentClient]. */
internal class EnrollmentHost(
    private val context: Context,
    private val checkpointStore: HostedSecureStore = AndroidHostedSecureStore(context),
    private val identityInstaller: (NativeEnrollmentIdentity) -> ImportResult = { identity ->
        NativeGateway.installEnrollmentIdentity(context, identity)
    },
    private val transport: EnrollmentTransport? = null,
    private val vaultSecrets: VaultSecretStore = VaultSecretStore(AndroidHostedSecureStore(context)),
) : AutoCloseable {
    enum class PhoneState { PENDING, CLAIMED, APPROVED, DENIED, EXPIRED }

    enum class PhoneClaimFailure { INVALID_QR, NETWORK, EXPIRED, REJECTED }

    enum class VaultRequestState { PENDING, APPROVED, DENIED, EXPIRED }

    /** Pending-secret state for creation resume: STORED uses the pending slot, PROMPT asks because it is missing, UNAVAILABLE fails closed. */
    enum class ResumeSecret { NONE, STORED, PROMPT, UNAVAILABLE }

    private enum class Operation { OWNER, MANUAL, VAULT_REQUEST }
    private data class ActiveClient(val origin: String, val operation: Operation, val client: NativeEnrollmentClient)

    private val hostedAccount by lazy { HostedAccountClient(AndroidHostedSecureStore(context)) }
    private val clientLock = Any()
    private var activeClient: ActiveClient? = null
    @Volatile private var lastErrorCode: String? = null
    @Volatile private var lastPhoneClaimFailure: PhoneClaimFailure? = null

    fun discover(origin: String): String? = request(origin, "GET", "/v1/instance", null)

    fun claimedState(discovery: String): Boolean? = parseInstanceDiscovery(discovery)?.claimed

    fun hasPendingOwner(origin: String): Boolean = try {
        checkpointStore.read(ownerCheckpointKey(origin)) != null
    } catch (_: Exception) {
        true
    }

    /** Pending-slot state for an unclaimed origin; a read failure fails closed and never prompts or regenerates. */
    fun ownerResumeSecret(origin: String): ResumeSecret = try {
        if (checkpointStore.read(ownerCheckpointKey(origin)) == null) ResumeSecret.NONE else pendingResumeState(origin)
    } catch (_: Exception) {
        ResumeSecret.UNAVAILABLE
    }

    /** Pending-slot state for an approved vault request; only prepared owner material needs the secret again. */
    fun vaultCreationSecret(origin: String): ResumeSecret = try {
        if (clientFor(origin, Operation.VAULT_REQUEST).hasPreparedOwnerMaterial()) pendingResumeState(origin) else ResumeSecret.NONE
    } catch (_: Exception) {
        ResumeSecret.UNAVAILABLE
    }

    private fun pendingResumeState(origin: String): ResumeSecret = when (val pending = vaultSecrets.readPending(origin)) {
        is StoredSecret.Present -> {
            pending.value.fill('\u0000')
            ResumeSecret.STORED
        }
        StoredSecret.Missing -> ResumeSecret.PROMPT
    }

    /**
     * [secret] is the entered secret, blank when the stored pending secret is used. Creation and legacy resume
     * require the entered secret to be durable before the checkpoint and claim request.
     */
    fun claimOwner(origin: String, username: String, setupToken: String, adminPassword: String, secret: String): ImportResult {
        lastErrorCode = null
        return try {
        val resumed = checkpointStore.read(ownerCheckpointKey(origin)) != null
        val client = clientFor(origin, Operation.OWNER)
        val passphrase = try {
            resolvePendingSecret(origin, client, resumed, secret)
        } catch (_: SecureStoreUnavailable) {
            lastErrorCode = "secure-store-unavailable"
            return ImportResult.STORAGE_ERROR
        }
        if (passphrase == null) return ImportResult.SERVER_REJECTED
        if (!vaultSecrets.savePending(origin, passphrase.toCharArray())) {
            lastErrorCode = "secure-store-unavailable"
            return ImportResult.STORAGE_ERROR
        }
        val body = client.createOwnerRequest(setupToken, username, adminPassword, passphrase)
        persistOwnerCheckpoint(origin, client.checkpoint())
        val response = claimRequest(origin, body)
            ?: return ImportResult.NETWORK_ERROR
        claimErrorResult(response)?.let { return it }
        if (recordError(response)) return ImportResult.SERVER_REJECTED
        val result = install(client.finishOwnerAdmission(response, passphrase), origin)
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) {
            clearOwnerCheckpoint(origin)
            closeActiveClient()
        }
        result
        } catch (_: Exception) {
            ImportResult.SERVER_REJECTED
        }
    }

    /**
     * Stored pending secret wins (desktop order). Without one, a resumed checkpoint needs the entered secret,
     * verified against the checkpoint before it is adopted; a fresh creation uses the entered secret.
     * Returns null with [lastErrorCode] set when the request must not proceed. Read failures throw [SecureStoreUnavailable].
     */
    private fun resolvePendingSecret(origin: String, client: NativeEnrollmentClient, resumed: Boolean, entered: String): String? =
        when (val pending = vaultSecrets.readPending(origin)) {
            is StoredSecret.Present -> {
                val stored = String(pending.value)
                pending.value.fill('\u0000')
                if (resumed && !client.ownerPassphraseMatches(stored)) {
                    lastErrorCode = "vault-secret-mismatch"
                    null
                } else {
                    stored
                }
            }
            StoredSecret.Missing -> when {
                !resumed -> entered.takeIf(String::isNotBlank)
                entered.isBlank() -> {
                    lastErrorCode = "owner-secret-required"
                    null
                }
                !client.ownerPassphraseMatches(entered) -> {
                    lastErrorCode = "wrong-passphrase"
                    null
                }
                else -> entered
            }
        }

    fun joinWithSecret(origin: String, vaultOwner: String?, secret: String): ImportResult {
        return try {
        lastErrorCode = null
        val client = clientFor(origin, Operation.MANUAL)
        val challengeResponse = enrollmentRequest(origin, "POST", "/v1/enrollment/challenges", client.manualChallengeRequest(null, vaultOwner))
            ?: return ImportResult.NETWORK_ERROR
        val challenge = challengeResponse.body ?: return ImportResult.NETWORK_ERROR
        if (!challengeResponse.isSuccess) {
            recordError(challenge)
            return if (challengeResponse.status in TRANSIENT_STATUSES) ImportResult.NETWORK_ERROR else ImportResult.SERVER_REJECTED
        }
        val enrollmentId = JSONObject(challenge).getString("enrollment_id")
        val completeBody = client.completeManualChallengeRequest(challenge, secret)
        val completeResponse = enrollmentRequest(origin, "POST", "/v1/enrollment/challenges/$enrollmentId/complete", completeBody)
            ?: return ImportResult.NETWORK_ERROR
        val complete = completeResponse.body ?: return ImportResult.NETWORK_ERROR
        if (!completeResponse.isSuccess) {
            recordError(complete)
            return if (completeResponse.status in TRANSIENT_STATUSES) ImportResult.NETWORK_ERROR else ImportResult.SERVER_REJECTED
        }
        val result = install(client.finishManualAdmission(complete, secret))
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) {
            val credential = NativeGateway.accountCredential(context)
            if (credential != null) vaultSecrets.saveIfMissing(credential.vaultId, credential.deviceId, secret.toCharArray())
        }
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) closeActiveClient()
        result
        } catch (_: Exception) {
            ImportResult.SERVER_REJECTED
        }
    }

    fun signIn(origin: String, username: String, password: String): Boolean {
        lastErrorCode = null
        return try {
            val body = JSONObject().put("username", username).put("admin_password", password).put("client", "android").toString()
            val response = enrollmentRequest(origin, "POST", "/v1/instance/session", body) ?: return false
            if (response.isSuccess) return true
            response.body?.let(::recordError)
            false
        } catch (_: Exception) { false }
    }

    fun parseVaultInviteLink(link: String): String? = try { parseNativeVaultInviteLink(link).origin } catch (_: Exception) { null }

    fun beginVaultRequest(expectedOrigin: String, link: String, displayName: String): VaultRequestState? {
        lastErrorCode = null
        return try {
        val invite = parseNativeVaultInviteLink(link)
        if (invite.origin != expectedOrigin) {
            lastErrorCode = "invite_invalid"
            return null
        }
        val client = clientFor(invite.origin, Operation.VAULT_REQUEST)
        val body = client.beginVaultRequest(invite.origin, invite.inviteToken, displayName)
        persistVaultRequestCheckpoint(invite.origin, client.checkpoint())
        val response = enrollmentRequest(invite.origin, "POST", "/v1/instance/vault-requests", body) ?: return null
        val responseBody = response.body ?: return null
        if (!response.isSuccess) {
            if (recordError(responseBody) && response.status !in TRANSIENT_STATUSES) clearRejectedVaultRequest(invite.origin)
            return null
        }
        client.acceptVaultRequestCreated(responseBody)
        persistVaultRequestCheckpoint(invite.origin, client.checkpoint())
        VaultRequestState.PENDING
        } catch (_: Exception) { null }
    }

    fun pendingVaultRequest(origin: String): Boolean = try {
        checkpointStore.read(vaultRequestCheckpointKey(origin)) != null
    } catch (_: Exception) {
        lastErrorCode = "storage_error"
        false
    }

    fun vaultRequestStatus(origin: String, knownState: VaultRequestState = VaultRequestState.PENDING): VaultRequestState? {
        lastErrorCode = null
        return try {
        val client = clientFor(origin, Operation.VAULT_REQUEST)
        val pending = client.pendingVaultRequest() ?: return null
        if (!client.vaultRequestAccepted()) {
            val submission = client.beginVaultRequest(origin, "", "")
            val submitted = enrollmentRequest(origin, "POST", "/v1/instance/vault-requests", submission) ?: return null
            val submittedBody = submitted.body ?: return null
            if (!submitted.isSuccess) {
                if (recordError(submittedBody) && submitted.status !in TRANSIENT_STATUSES) clearRejectedVaultRequest(origin)
                return null
            }
            client.acceptVaultRequestCreated(submittedBody)
            persistVaultRequestCheckpoint(origin, client.checkpoint())
        }
        val headers = JSONObject()
            .put("Peppy-Vault-Request", client.vaultRequestHeader())
            .put("Peppy-Known-State", knownState.name.lowercase())
        val response = enrollmentRequest(origin, "GET", "/v1/instance/vault-requests/${pending.requestId}?wait=15", null, headers) ?: return null
        val responseBody = response.body ?: return null
        if (!response.isSuccess) {
            if (response.status == 404 && recordError(responseBody)) {
                if (lastErrorCode == "request_not_found") clearRejectedVaultRequest(origin)
                return VaultRequestState.EXPIRED
            }
            recordError(responseBody)
            return null
        }
        val state = when (client.parseVaultRequestStatus(responseBody)) {
            "pending" -> VaultRequestState.PENDING
            "approved" -> VaultRequestState.APPROVED
            "denied" -> VaultRequestState.DENIED
            "expired" -> VaultRequestState.EXPIRED
            "completed" -> if (client.hasPreparedOwnerMaterial()) VaultRequestState.APPROVED else {
                lastErrorCode = "request_not_approved"
                VaultRequestState.EXPIRED
            }
            else -> null
        }
        if (state == VaultRequestState.DENIED || state == VaultRequestState.EXPIRED) clearRejectedVaultRequest(origin)
        state
        } catch (_: Exception) { null }
    }

    /** [secret] is the entered secret, blank when the stored pending secret is used (desktop order: stored, then prompt). */
    fun createVault(origin: String, username: String, adminPassword: String, secret: String): ImportResult {
        lastErrorCode = null
        return try {
        val client = clientFor(origin, Operation.VAULT_REQUEST)
        val passphrase = try {
            resolvePendingSecret(origin, client, client.hasPreparedOwnerMaterial(), secret)
        } catch (_: SecureStoreUnavailable) {
            lastErrorCode = "secure-store-unavailable"
            return ImportResult.STORAGE_ERROR
        }
        if (passphrase == null) return ImportResult.SERVER_REJECTED
        if (!vaultSecrets.savePending(origin, passphrase.toCharArray())) {
            lastErrorCode = "secure-store-unavailable"
            return ImportResult.STORAGE_ERROR
        }
        val body = client.createVaultRequest(username, adminPassword, passphrase)
        persistVaultRequestCheckpoint(origin, client.checkpoint())
        val response = enrollmentRequest(origin, "POST", "/v1/instance/vaults", body) ?: return ImportResult.NETWORK_ERROR
        val responseBody = response.body ?: return ImportResult.NETWORK_ERROR
        if (!response.isSuccess) {
            recordError(responseBody)
            return if (response.status in TRANSIENT_STATUSES) ImportResult.NETWORK_ERROR else ImportResult.SERVER_REJECTED
        }
        val result = install(client.finishOwnerAdmission(responseBody, passphrase), origin)
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) {
            client.clearVaultRequest()
            clearVaultRequestCheckpoint(origin)
            closeActiveClient()
        }
        result
        } catch (_: Exception) { ImportResult.SERVER_REJECTED }
    }

    fun cancelVaultRequest(origin: String): Boolean {
        try {
            if (clientFor(origin, Operation.VAULT_REQUEST).hasPreparedOwnerMaterial()) return true
        } catch (_: Exception) { return false }
        try { clientFor(origin, Operation.VAULT_REQUEST).clearVaultRequest() } catch (_: Exception) { return false }
        clearVaultRequestCheckpoint(origin)
        closeActiveClient()
        return true
    }

    fun takeErrorCode(): String? = lastErrorCode.also { lastErrorCode = null }

    fun takePhoneClaimFailure(): PhoneClaimFailure? = lastPhoneClaimFailure.also { lastPhoneClaimFailure = null }

    private fun clearRejectedVaultRequest(origin: String) {
        try { clientFor(origin, Operation.VAULT_REQUEST).clearVaultRequest() } catch (_: Exception) { }
        clearVaultRequestCheckpoint(origin)
        closeActiveClient()
    }

    fun beginPhoneClaim(qr: String): PhoneEnrollment? {
        lastPhoneClaimFailure = null
        val origin = try { JSONObject(qr).getString("origin") } catch (_: Exception) {
            return failedPhoneClaim(null, PhoneClaimFailure.INVALID_QR)
        }
        val client = try { newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "Android phone") } catch (_: Exception) {
            return failedPhoneClaim(null, PhoneClaimFailure.INVALID_QR)
        }
        val request = try { client.claimPhoneInvitationRequest(qr) } catch (_: Exception) {
            return failedPhoneClaim(client, PhoneClaimFailure.INVALID_QR)
        }
        return try {
            val response = enrollmentRequest(origin, "POST", "/v1/enrollment/invitations/claim", request)
                ?: return failedPhoneClaim(client, PhoneClaimFailure.NETWORK)
            if (!response.isSuccess) return failedPhoneClaim(client, phoneClaimFailure(response.status))
            val claim = response.body ?: return failedPhoneClaim(client, PhoneClaimFailure.REJECTED)
            client.consumePhoneClaim(claim)
            val method = JSONObject(claim).getString("method")
            if (method != "transfer" && method != "shared_secret") throw IllegalArgumentException("Unsupported enrollment method")
            PhoneEnrollment(origin, client, method == "shared_secret")
        } catch (_: Exception) {
            failedPhoneClaim(client, PhoneClaimFailure.REJECTED)
        }
    }

    internal inner class PhoneEnrollment(
        private val origin: String,
        private val client: NativeEnrollmentClient,
        private val sharedSecret: Boolean,
    ) : AutoCloseable {
        @Volatile private var closed = false

        fun requiresSharedSecret(): Boolean = sharedSecret

        fun status(): String? = try {
            val descriptor = JSONObject(client.phoneStatusRequest())
            val path = descriptor.getString("path")
            val headers = descriptor.getJSONObject("headers")
            if (descriptor.length() != 3 || descriptor.getString("method") != "GET" ||
                headers.length() != 1 || !headers.has("Peppy-Claim-Secret") || headers.getString("Peppy-Claim-Secret").isEmpty() ||
                !path.startsWith("/v1/enrollment/invitations/") || path.drop("/v1/enrollment/invitations/".length).contains('/') ||
                path.contains('?') || path.contains('#')
            ) null else request(origin, "GET", path, null, headers)
        } catch (_: Exception) {
            null
        }

        fun state(status: String): PhoneState? = try {
            when (JSONObject(status).getString("state")) {
                "pending" -> PhoneState.PENDING
                "claimed" -> PhoneState.CLAIMED
                "approved" -> PhoneState.APPROVED
                "denied" -> PhoneState.DENIED
                "expired" -> PhoneState.EXPIRED
                else -> null
            }
        } catch (_: Exception) {
            null
        }

        fun sas(status: String): String? = try { client.phoneSas(status) } catch (_: Exception) { null }

        fun confirm(status: String, secret: String?): ImportResult {
            if (sharedSecret && secret.isNullOrBlank()) return ImportResult.SERVER_REJECTED
            return try {
                val descriptor = JSONObject(client.phoneStatusRequest())
                val path = descriptor.getString("path")
                val complete = request(
                    origin,
                    "POST",
                    "$path/confirm",
                    client.confirmPhoneInvitationRequest(status, secret),
                    descriptor.getJSONObject("headers"),
                ) ?: return ImportResult.NETWORK_ERROR
                val result = synchronized(this) {
                    if (closed) return ImportResult.SERVER_REJECTED
                    install(client.finishPhoneAdmission(complete))
                }
                if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) close()
                result
            } catch (_: Exception) {
                ImportResult.SERVER_REJECTED
            }
        }

        @Synchronized override fun close() {
            if (closed) return
            closed = true
            try { client.cancelPhoneFlow() } catch (_: Exception) { }
            client.close()
        }
    }

    override fun close() {
        closeActiveClient()
    }

    private fun clientFor(origin: String, operation: Operation): NativeEnrollmentClient = synchronized(clientLock) {
        activeClient?.let { active ->
            if (active.origin == origin && active.operation == operation) return@synchronized active.client
            closeActiveClientLocked()
        }
        val restored = when (operation) {
            Operation.OWNER -> restoreOwnerClient(origin)
            Operation.VAULT_REQUEST -> restoreVaultRequestClient(origin)
            Operation.MANUAL -> null
        }
        val client = restored ?: newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "Android phone")
        activeClient = ActiveClient(origin, operation, client)
        client
    }

    private fun restoreOwnerClient(origin: String): NativeEnrollmentClient? {
        val checkpoint = readOwnerCheckpoint(origin) ?: return null
        return restoreNativeEnrollmentClient(checkpoint)
    }

    private fun restoreVaultRequestClient(origin: String): NativeEnrollmentClient? {
        val checkpoint = readVaultRequestCheckpoint(origin) ?: return null
        return restoreNativeEnrollmentClient(checkpoint)
    }

    private fun persistOwnerCheckpoint(origin: String, checkpoint: ByteArray) {
        val envelope = JSONObject()
            .put("version", 1)
            .put("origin", origin)
            .put("checkpoint", Base64.getEncoder().encodeToString(checkpoint))
            .toString()
            .toByteArray(Charsets.UTF_8)
        checkpointStore.write(ownerCheckpointKey(origin), envelope)
    }

    private fun readOwnerCheckpoint(origin: String): ByteArray? {
        val value = checkpointStore.read(ownerCheckpointKey(origin)) ?: return null
        val envelope = JSONObject(value.toString(Charsets.UTF_8))
        check(envelope.getInt("version") == 1 && envelope.getString("origin") == origin)
        return Base64.getDecoder().decode(envelope.getString("checkpoint"))
    }

    private fun clearOwnerCheckpoint(origin: String) {
        try { checkpointStore.remove(ownerCheckpointKey(origin)) } catch (_: Exception) { }
    }

    private fun persistVaultRequestCheckpoint(origin: String, checkpoint: ByteArray) =
        persistCheckpoint(vaultRequestCheckpointKey(origin), origin, checkpoint)

    private fun readVaultRequestCheckpoint(origin: String): ByteArray? =
        readCheckpoint(vaultRequestCheckpointKey(origin), origin)

    private fun clearVaultRequestCheckpoint(origin: String) {
        try { checkpointStore.remove(vaultRequestCheckpointKey(origin)) } catch (_: Exception) { }
    }

    private fun ownerCheckpointKey(origin: String): String {
        return checkpointKey(OWNER_CHECKPOINT_PREFIX, origin)
    }

    private fun vaultRequestCheckpointKey(origin: String): String = checkpointKey(VAULT_REQUEST_CHECKPOINT_PREFIX, origin)

    private fun checkpointKey(prefix: String, origin: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(origin.toByteArray(Charsets.UTF_8))
        return prefix + digest.joinToString("") { byte -> "%02x".format(byte) }
    }

    private fun persistCheckpoint(key: String, origin: String, checkpoint: ByteArray) = synchronized(clientLock) {
        val envelope = JSONObject().put("version", 1).put("origin", origin)
            .put("checkpoint", Base64.getEncoder().encodeToString(checkpoint)).toString().toByteArray(Charsets.UTF_8)
        checkpointStore.write(key, envelope)
    }

    private fun readCheckpoint(key: String, origin: String): ByteArray? = synchronized(clientLock) {
        val value = checkpointStore.read(key) ?: return null
        val envelope = JSONObject(value.toString(Charsets.UTF_8))
        check(envelope.getInt("version") == 1 && envelope.getString("origin") == origin)
        return Base64.getDecoder().decode(envelope.getString("checkpoint"))
    }

    private fun recordError(response: String): Boolean {
        val code = try { JSONObject(response).optString("code").takeIf(String::isNotBlank) } catch (_: Exception) { null }
        lastErrorCode = code
        return code != null
    }

    private fun closeActiveClient() = synchronized(clientLock) { closeActiveClientLocked() }

    private fun closeActiveClientLocked() {
        activeClient?.client?.close()
        activeClient = null
    }

    private fun install(identity: NativeEnrollmentIdentity, pendingOrigin: String? = null): ImportResult = try {
        val metadata = identity.metadata()
        identityInstaller(identity).also { result ->
            if ((result == ImportResult.IMPORTED || result == ImportResult.UPDATED) && pendingOrigin != null) {
                try { vaultSecrets.movePendingToBinding(pendingOrigin, metadata.vaultId, metadata.deviceId) } catch (_: Exception) { }
            }
        }
    } finally {
        identity.close()
    }

    private fun failedPhoneClaim(client: NativeEnrollmentClient?, failure: PhoneClaimFailure): PhoneEnrollment? {
        lastPhoneClaimFailure = failure
        try { client?.close() } catch (_: Exception) { }
        return null
    }

    private fun phoneClaimFailure(status: Int): PhoneClaimFailure = when {
        status == 404 || status == 410 -> PhoneClaimFailure.EXPIRED
        status == 429 || status >= 500 -> PhoneClaimFailure.NETWORK
        else -> PhoneClaimFailure.REJECTED
    }

    /** Legacy phone/claim callers deliberately see only 2xx bodies. */
    private fun request(origin: String, method: String, path: String, body: String?, headers: JSONObject? = null): String? =
        enrollmentRequest(origin, method, path, body, headers)?.takeIf { it.isSuccess }?.body

    private fun enrollmentRequest(origin: String, method: String, path: String, body: String?, headers: JSONObject? = null): EnrollmentHttpResponse? {
        if (CredentialParser.canonicalOrigin(origin) != origin || !path.startsWith("/v1/") || path.contains('#') || (path.contains('?') && !path.endsWith("?wait=15"))) return null
        transport?.let { return it.request(origin, method, path, body, headers) }
        val connection = try { URL(origin + path).openConnection() as HttpURLConnection } catch (_: Exception) { return null }
        return try {
            connection.instanceFollowRedirects = false
            connection.requestMethod = method
            connection.connectTimeout = ENROLLMENT_TIMEOUT_MS
            connection.readTimeout = ENROLLMENT_TIMEOUT_MS
            connection.setRequestProperty("Accept", "application/json")
            hostedAccountHeader(origin)?.let { connection.setRequestProperty("Peppy-Account-Session", it) }
            headers?.keys()?.forEach { name -> connection.setRequestProperty(name, headers.getString(name)) }
            body?.let { value ->
                val bytes = value.toByteArray()
                connection.doOutput = true
                connection.setFixedLengthStreamingMode(bytes.size)
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(bytes) }
            }
            val status = connection.responseCode
            val stream = if (status in 200..299) connection.inputStream else connection.errorStream
            EnrollmentHttpResponse(status, stream?.use { GatewayHttp.readBounded(it, 64 * 1024) })
        } catch (_: Exception) {
            null
        } finally {
            connection.disconnect()
        }
    }

    private fun claimRequest(origin: String, body: String): String? {
        if (CredentialParser.canonicalOrigin(origin) != origin) return null
        transport?.let { return it.request(origin, "POST", "/v1/instance/claim", body, null)?.takeIf(EnrollmentHttpResponse::isSuccess)?.body }
        val connection = try { URL(origin + "/v1/instance/claim").openConnection() as HttpURLConnection } catch (_: Exception) { return null }
        return try {
            connection.instanceFollowRedirects = false
            connection.requestMethod = "POST"
            connection.connectTimeout = GatewayHttp.TIMEOUT_MS
            connection.readTimeout = GatewayHttp.TIMEOUT_MS
            connection.setRequestProperty("Accept", "application/json")
            connection.setRequestProperty("Content-Type", "application/json")
            hostedAccountHeader(origin)?.let { connection.setRequestProperty("Peppy-Account-Session", it) }
            val bytes = body.toByteArray()
            connection.doOutput = true
            connection.setFixedLengthStreamingMode(bytes.size)
            connection.outputStream.use { it.write(bytes) }
            val successful = connection.responseCode in 200..299
            val stream = if (successful) connection.inputStream else connection.errorStream
            stream?.use { GatewayHttp.readBounded(it, if (successful) 64 * 1024 else MAX_CLAIM_ERROR_BYTES) }
        } catch (_: Exception) {
            null
        } finally {
            connection.disconnect()
        }
    }

    private fun hostedAccountHeader(origin: String): String? =
        if (origin == HostedAccountClient.ORIGIN) hostedAccount.enrollmentSessionHeader() else null

    private companion object {
        const val OWNER_CHECKPOINT_PREFIX = "self-hosted-owner-enrollment-checkpoint-v1-"
        const val VAULT_REQUEST_CHECKPOINT_PREFIX = "vault-request:v1:"
        const val ENROLLMENT_TIMEOUT_MS = 20_000
        val TRANSIENT_STATUSES = setOf(408, 429, 500, 502, 503, 504)
    }
}

private val EnrollmentHttpResponse.isSuccess: Boolean get() = status in 200..299
