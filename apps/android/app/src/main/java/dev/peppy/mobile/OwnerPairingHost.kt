package dev.peppy.mobile

import android.content.Context
import org.json.JSONException
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.pairingIntentSas
import uniffi.peppy_mobile_bindings.intentDigestHex
import uniffi.peppy_mobile_bindings.parseJoinRequestQr
import uniffi.peppy_mobile_bindings.sealIntentToken
import uniffi.peppy_mobile_bindings.vaultProfileFingerprint
import java.net.URI
import java.util.UUID

internal class OwnerPairingIntent(
    val token: String,
    val origin: String,
    val vaultId: String,
    val deviceId: String,
    val profileFingerprint: String,
    val keyEpoch: Long,
    val expiresAtMs: Long,
) {
    override fun toString() = "OwnerPairingIntent(redacted)"
}

internal data class OwnerPairingClaim(val deviceId: String, val digest: String, val sas: String, val requestedRole: String)
internal data class OwnerPairingFacts(val origin: String, val vaultId: String, val deviceId: String, val profileJson: String)

/** Join QR material intentionally excludes the intent token. */
internal data class OwnerPairingJoinRequest(val httpsOrigin: String, val joinRequestId: String, val joinKey: String)

internal interface OwnerPairingJoinRequestCrypto {
    fun parse(payload: String, allowLoopbackHttp: Boolean): OwnerPairingJoinRequest?
    fun seal(joinKey: String, intentToken: String): String?
    fun digest(intentToken: String): String?
}

internal enum class OwnerPairingComputerError { ORIGIN_MISMATCH, EXPIRED, ALREADY_LINKED, PAIRING, NETWORK, PAUSED }
internal sealed interface OwnerPairingComputerResult {
    data class Ready(val intent: OwnerPairingIntent) : OwnerPairingComputerResult
    data class Error(val error: OwnerPairingComputerError) : OwnerPairingComputerResult
}

/** Test fakes receive facts, never a bearer. Production transport resolves the bearer internally. */
internal interface OwnerPairingTransport {
    fun get(facts: OwnerPairingFacts, path: String): HttpResult?
    fun post(facts: OwnerPairingFacts, path: String, body: String): HttpResult?
}

internal class OwnerPairingHost(
    private val facts: () -> OwnerPairingFacts?,
    private val transport: OwnerPairingTransport,
    private val joinCrypto: OwnerPairingJoinRequestCrypto = NativeOwnerPairingJoinRequestCrypto,
) {
    private val tokenPattern = Regex("[A-Za-z0-9_-]{43}")
    private val digestPattern = Regex("[0-9a-f]{64}")

    private fun json(result: HttpResult?): JSONObject? = result?.takeIf { it.ok }?.body?.let {
        try { JSONObject(it) } catch (_: JSONException) { null }
    }

    fun create(): OwnerPairingIntent? {
        val local = verifiedLocal() ?: return null
        val vault = json(transport.get(local.facts, "/v1/vault")) ?: return null
        if (!matchesServerVault(vault, local)) return null
        val response = json(transport.post(local.facts, "/v1/pairing/intents", JSONObject().put("https_origin", local.facts.origin).toString())) ?: return null
        val origin = CredentialParser.canonicalOrigin(response.optString("https_origin"), false) ?: return null
        val token = response.optString("intent_token").takeIf { tokenPattern.matches(it) } ?: return null
        val ttl = response.optLong("expires_in_seconds", 0)
        if (origin != local.facts.origin || ttl !in 1..900) return null
        val expiresAtMs = System.currentTimeMillis() + ttl * 1000
        return OwnerPairingIntent(token, origin, local.facts.vaultId, local.facts.deviceId, local.fingerprint, local.epoch, expiresAtMs)
    }

    fun startComputerPairing(payload: String): OwnerPairingComputerResult {
        val request = joinCrypto.parse(payload, BuildConfig.DEBUG) ?: return OwnerPairingComputerResult.Error(OwnerPairingComputerError.PAIRING)
        val local = verifiedLocal() ?: return OwnerPairingComputerResult.Error(OwnerPairingComputerError.PAIRING)
        if (canonicalOrigin(request.httpsOrigin) != local.facts.origin) return OwnerPairingComputerResult.Error(OwnerPairingComputerError.ORIGIN_MISMATCH)
        val intent = create() ?: return OwnerPairingComputerResult.Error(OwnerPairingComputerError.NETWORK)
        val digest = joinCrypto.digest(intent.token) ?: return OwnerPairingComputerResult.Error(OwnerPairingComputerError.PAIRING)
        val sealed = joinCrypto.seal(request.joinKey, intent.token) ?: return OwnerPairingComputerResult.Error(OwnerPairingComputerError.PAIRING)
        val body = JSONObject().put("intent_digest", digest).put("sealed_intent_token", sealed).toString()
        val result = try { transport.post(local.facts, "/v1/pairing/join-requests/${request.joinRequestId}/offer", body) } catch (_: Exception) { null }
        if (result?.ok == true) return OwnerPairingComputerResult.Ready(intent)
        return OwnerPairingComputerResult.Error(when {
            result == null -> OwnerPairingComputerError.NETWORK
            result.code == 410 && result.errorCode == "join_request_expired" -> OwnerPairingComputerError.EXPIRED
            result.code == 404 -> OwnerPairingComputerError.EXPIRED
            result.code == 409 && result.errorCode == "join_request_already_offered" -> OwnerPairingComputerError.ALREADY_LINKED
            result.code == 409 && result.errorCode == "pairing_intent_not_offerable" -> OwnerPairingComputerError.PAIRING
            else -> OwnerPairingComputerError.NETWORK
        })
    }

    /** A null result means unclaimed, expired, stale, or invalid; callers must clear confirmation. */
    fun status(intent: OwnerPairingIntent): OwnerPairingClaim? {
        val local = verifiedLocal(intent) ?: return null
        val vault = json(transport.get(local.facts, "/v1/vault")) ?: return null
        if (!matchesServerVault(vault, local)) return null
        val claimJson = json(transport.get(local.facts, "/v1/pairing/intents/${intent.token}")) ?: return null
        if (claimJson.optLong("expires_in_seconds", 0) !in 1..900 || !claimJson.optBoolean("claimed") || claimJson.optBoolean("approved")) return null
        val device = claimJson.optString("device_id").takeIf(::isUuid) ?: return null
        val digest = claimJson.optString("key_digest").takeIf { digestPattern.matches(it) } ?: return null
        val requestedRole = claimJson.optString("requested_role").takeIf { it in setOf("device", "gateway") } ?: return null
        val localSas = try { pairingIntentSas(intent.token, digest, device) } catch (_: Exception) { return null }
        if (claimJson.optString("sas") != localSas) return null
        return OwnerPairingClaim(device, digest, localSas, requestedRole)
    }

    fun approve(intent: OwnerPairingIntent, approved: OwnerPairingClaim, confirmed: Boolean, expectedRole: String? = null, canContinue: () -> Boolean = { true }): Boolean {
        if (!confirmed || !canContinue() || System.currentTimeMillis() >= intent.expiresAtMs) return false
        if (expectedRole != null && approved.requestedRole != expectedRole) return false
        val current = status(intent) ?: return false
        if (current != approved || !canContinue()) return false
        val local = verifiedLocal(intent) ?: return false
        val vault = json(transport.get(local.facts, "/v1/vault")) ?: return false
        if (!matchesServerVault(vault, local)) return false
        val body = JSONObject().put("key_digest", approved.digest).put("profile_fingerprint", local.fingerprint).put("key_epoch", local.epoch).toString()
        if (!canContinue() || System.currentTimeMillis() >= intent.expiresAtMs) return false
        return json(transport.post(local.facts, "/v1/pairing/intents/${intent.token}/approve", body)) != null
    }

    private class Verified(val facts: OwnerPairingFacts, val fingerprint: String, val epoch: Long)
    private fun verifiedLocal(intent: OwnerPairingIntent? = null): Verified? {
        val current = facts() ?: return null
        val origin = canonicalOrigin(current.origin) ?: return null
        if (origin != current.origin) return null
        val epoch = try { JSONObject(current.profileJson).getLong("key_epoch") } catch (_: Exception) { return null }
        val fingerprint = try { vaultProfileFingerprint(current.profileJson) } catch (_: Exception) { return null }
        if (epoch !in 1..0xffff_ffffL || !digestPattern.matches(fingerprint)) return null
        if (intent != null && (intent.origin != origin || intent.vaultId != current.vaultId || intent.deviceId != current.deviceId || intent.profileFingerprint != fingerprint || intent.keyEpoch != epoch)) return null
        return Verified(current, fingerprint, epoch)
    }

    private fun matchesServerVault(vault: JSONObject, local: Verified): Boolean = try {
        vault.getString("role") == "owner" && vault.getString("vault_id") == local.facts.vaultId &&
            vault.getString("device_id") == local.facts.deviceId && vault.getLong("key_epoch") == local.epoch &&
            vault.getString("profile_fingerprint") == local.fingerprint
    } catch (_: Exception) { false }

    private fun canonicalOrigin(raw: String): String? = CredentialParser.canonicalOrigin(raw, false)

    private fun isUuid(value: String) = try { UUID.fromString(value).toString() == value.lowercase() } catch (_: Exception) { false }

    companion object {
        fun production(context: Context) = OwnerPairingHost(
            facts = {
                NativeGateway.ownerPairingLocalMaterial(context)?.let { OwnerPairingFacts(it.origin, it.vaultId, it.deviceId, it.profileJson) }
            },
            transport = object : OwnerPairingTransport {
                private fun http(facts: OwnerPairingFacts): GatewayHttp? {
                    val credential = NativeGateway.accountCredential(context) ?: return null
                    if (credential.origin != facts.origin || credential.vaultId != facts.vaultId || credential.deviceId != facts.deviceId) return null
                    return GatewayHttp(credential.origin, credential.bearerToken)
                }
                override fun get(facts: OwnerPairingFacts, path: String) = try { http(facts)?.get(path) } catch (_: Exception) { null }
                override fun post(facts: OwnerPairingFacts, path: String, body: String) = try { http(facts)?.postJson(path, body) } catch (_: Exception) { null }
            },
        )
    }
}

private object NativeOwnerPairingJoinRequestCrypto : OwnerPairingJoinRequestCrypto {
    override fun parse(payload: String, allowLoopbackHttp: Boolean) = try {
        parseJoinRequestQr(payload, allowLoopbackHttp).let { OwnerPairingJoinRequest(it.httpsOrigin, it.joinRequestId, it.joinKey) }
    } catch (_: Exception) { null }
    override fun seal(joinKey: String, intentToken: String) = try { sealIntentToken(joinKey, intentToken) } catch (_: Exception) { null }
    override fun digest(intentToken: String) = try { intentDigestHex(intentToken) } catch (_: Exception) { null }
}
