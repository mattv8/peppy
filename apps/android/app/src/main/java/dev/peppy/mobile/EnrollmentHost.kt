package dev.peppy.mobile

import android.content.Context
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeEnrollmentClient
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.NativeEnrollmentIdentity
import uniffi.peppy_mobile_bindings.newNativeEnrollmentClient
import uniffi.peppy_mobile_bindings.restoreNativeEnrollmentClient
import java.net.HttpURLConnection
import java.net.URL
import java.util.Base64
import java.security.MessageDigest

internal fun interface EnrollmentTransport {
    fun request(origin: String, method: String, path: String, body: String?, headers: JSONObject?): String?
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
) : AutoCloseable {
    enum class PhoneState { PENDING, CLAIMED, APPROVED, DENIED, EXPIRED }

    private enum class Operation { OWNER, MANUAL }
    private data class ActiveClient(val origin: String, val operation: Operation, val client: NativeEnrollmentClient)

    private val hostedAccount by lazy { HostedAccountClient(AndroidHostedSecureStore(context)) }
    private var activeClient: ActiveClient? = null

    fun discover(origin: String): String? = request(origin, "GET", "/v1/instance", null)

    fun claimedState(discovery: String): Boolean? = parseInstanceDiscovery(discovery)?.claimed

    fun hasPendingOwner(origin: String): Boolean = try {
        checkpointStore.read(ownerCheckpointKey(origin)) != null
    } catch (_: Exception) {
        true
    }

    fun claimOwner(origin: String, setupToken: String, adminPassword: String, secret: String): ImportResult = try {
        val client = clientFor(origin, Operation.OWNER)
        val body = client.createOwnerRequest(setupToken, adminPassword, secret)
        persistOwnerCheckpoint(origin, client.checkpoint())
        val response = claimRequest(origin, body)
            ?: return ImportResult.NETWORK_ERROR
        claimErrorResult(response)?.let { return it }
        val result = install(client.finishOwnerAdmission(response, secret))
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) {
            clearOwnerCheckpoint(origin)
            closeActiveClient()
        }
        result
    } catch (_: Exception) {
        ImportResult.SERVER_REJECTED
    }

    fun joinWithSecret(origin: String, secret: String): ImportResult = try {
        val client = clientFor(origin, Operation.MANUAL)
        val challenge = request(origin, "POST", "/v1/enrollment/challenges", client.manualChallengeRequest(null))
            ?: return ImportResult.NETWORK_ERROR
        val enrollmentId = JSONObject(challenge).getString("enrollment_id")
        val completeBody = client.completeManualChallengeRequest(challenge, secret)
        val complete = request(origin, "POST", "/v1/enrollment/challenges/$enrollmentId/complete", completeBody)
            ?: return ImportResult.NETWORK_ERROR
        val result = install(client.finishManualAdmission(complete, secret))
        if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) closeActiveClient()
        result
    } catch (_: Exception) {
        ImportResult.SERVER_REJECTED
    }

    fun beginPhoneClaim(qr: String): PhoneEnrollment? {
        val origin = try { JSONObject(qr).getString("origin") } catch (_: Exception) { return null }
        val client = try { newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "Android phone") } catch (_: Exception) { return null }
        return try {
            val claim = request(origin, "POST", "/v1/enrollment/invitations/claim", client.claimPhoneInvitationRequest(qr))
                ?: run { client.close(); return null }
            client.consumePhoneClaim(claim)
            val method = JSONObject(claim).getString("method")
            if (method != "transfer" && method != "shared_secret") throw IllegalArgumentException("Unsupported enrollment method")
            PhoneEnrollment(origin, client, method == "shared_secret")
        } catch (_: Exception) {
            client.close()
            null
        }
    }

    internal inner class PhoneEnrollment(
        private val origin: String,
        private val client: NativeEnrollmentClient,
        private val sharedSecret: Boolean,
    ) : AutoCloseable {
        private var closed = false

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
                val result = install(client.finishPhoneAdmission(complete))
                if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) close()
                result
            } catch (_: Exception) {
                ImportResult.SERVER_REJECTED
            }
        }

        override fun close() {
            if (closed) return
            closed = true
            try { client.cancelPhoneFlow() } catch (_: Exception) { }
            client.close()
        }
    }

    override fun close() {
        closeActiveClient()
    }

    private fun clientFor(origin: String, operation: Operation): NativeEnrollmentClient {
        activeClient?.let { active ->
            if (active.origin == origin && active.operation == operation) return active.client
            closeActiveClient()
        }
        val client = if (operation == Operation.OWNER) restoreOwnerClient(origin)
            ?: newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "Android phone")
        else newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "Android phone")
        activeClient = ActiveClient(origin, operation, client)
        return client
    }

    private fun restoreOwnerClient(origin: String): NativeEnrollmentClient? {
        val checkpoint = readOwnerCheckpoint(origin) ?: return null
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

    private fun ownerCheckpointKey(origin: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(origin.toByteArray(Charsets.UTF_8))
        return OWNER_CHECKPOINT_PREFIX + digest.joinToString("") { byte -> "%02x".format(byte) }
    }

    private fun closeActiveClient() {
        activeClient?.client?.close()
        activeClient = null
    }

    private fun install(identity: NativeEnrollmentIdentity): ImportResult = try {
        identityInstaller(identity)
    } finally {
        identity.close()
    }

    private fun request(origin: String, method: String, path: String, body: String?, headers: JSONObject? = null): String? {
        if (CredentialParser.canonicalOrigin(origin) != origin || !path.startsWith("/v1/") || path.contains('?') || path.contains('#')) return null
        transport?.let { return it.request(origin, method, path, body, headers) }
        val connection = try { URL(origin + path).openConnection() as HttpURLConnection } catch (_: Exception) { return null }
        return try {
            connection.instanceFollowRedirects = false
            connection.requestMethod = method
            connection.connectTimeout = GatewayHttp.TIMEOUT_MS
            connection.readTimeout = GatewayHttp.TIMEOUT_MS
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
            if (connection.responseCode in 200..299) connection.inputStream.use { GatewayHttp.readBounded(it, 64 * 1024) } else null
        } catch (_: Exception) {
            null
        } finally {
            connection.disconnect()
        }
    }

    private fun claimRequest(origin: String, body: String): String? {
        if (CredentialParser.canonicalOrigin(origin) != origin) return null
        transport?.let { return it.request(origin, "POST", "/v1/instance/claim", body, null) }
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
    }
}
