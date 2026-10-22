package dev.peppy.mobile

import android.content.SharedPreferences
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.UUID

/** Identity-fenced protected relay bookkeeping. None of these values are UI DTOs. */
internal class RelayWakeIdentity(val origin: String, val vaultId: String, val deviceId: String, val provider: String, val environment: String, val credentialFingerprint: String) {
    fun matches(other: RelayWakeIdentity) = origin == other.origin && vaultId == other.vaultId && deviceId == other.deviceId && provider == other.provider && environment == other.environment && credentialFingerprint == other.credentialFingerprint
}

internal class RelayWakeCandidate(val registrationId: String?, val route: RelayRoute, val tokenFingerprint: String, val challenge: String? = null) {
    override fun toString() = "RelayWakeCandidate(redacted)"
}

internal class RelayWakeCleanup(val identity: RelayWakeIdentity, val route: RelayRoute, val bearerToken: String, val relayOrigin: String = identity.origin) {
    override fun toString() = "RelayWakeCleanup(redacted)"
}

/** Keystore-sealed atomic slots; commit before every network effect. */
internal class RelayWakeStateStore(private val prefs: SharedPreferences, private val box: SecretBox = KeystoreSecretBox) : RelayWakeCoordinatorStore {
    fun tokenFingerprint(token: String): String = MessageDigest.getInstance("SHA-256").digest(token.toByteArray()).joinToString("") { "%02x".format(it) }
    fun newCandidate(token: String) = RelayWakeCandidate(null, RelayRoute(UUID.randomUUID().toString(), secret(), secret()), tokenFingerprint(token))
    fun saveCandidate(candidate: RelayWakeCandidate): Boolean = save(CANDIDATE, encodeCandidate(candidate))
    fun candidate(): RelayWakeCandidate? = read(CANDIDATE)?.let(::decodeCandidate)
    fun clearCandidate(): Boolean = prefs.edit().remove(CANDIDATE).commit()
    fun saveActive(route: RelayRoute, fingerprint: String): Boolean = save(ACTIVE, "$fingerprint\n${route.routeId}\n${route.manageCredential}\n${route.wakeCredential}")
    fun active(): Pair<String, RelayRoute>? = read(ACTIVE)?.split('\n')?.takeIf { it.size == 4 }?.let { it[0] to RelayRoute(it[1], it[2], it[3]) }
    fun clearActive(): Boolean = prefs.edit().remove(ACTIVE).commit()
    /** Cleanup records are route-keyed so a new rotation cannot erase an older revocation. */
    fun saveCleanup(cleanup: RelayWakeCleanup): Boolean = save(cleanupKey(cleanup.route.routeId), listOf(cleanup.identity.origin, cleanup.identity.vaultId, cleanup.identity.deviceId, cleanup.identity.provider, cleanup.identity.environment, cleanup.identity.credentialFingerprint, cleanup.route.routeId, cleanup.route.manageCredential, cleanup.route.wakeCredential, cleanup.bearerToken, cleanup.relayOrigin).joinToString("\n"))
    fun cleanups(): List<RelayWakeCleanup> = prefs.all.keys.filter { it.startsWith("$CLEANUP.") }.mapNotNull { key ->
        read(key)?.split('\n')?.takeIf { it.size == 11 }?.let { values -> RelayWakeCleanup(RelayWakeIdentity(values[0], values[1], values[2], values[3], values[4], values[5]), RelayRoute(values[6], values[7], values[8]), values[9], values[10]) }
    }
    fun cleanup(): RelayWakeCleanup? = cleanups().firstOrNull()
    fun clearCleanup(): Boolean = cleanup()?.let { clearCleanup(it.route.routeId) } ?: true
    fun clearCleanup(routeId: String): Boolean = prefs.edit().remove(cleanupKey(routeId)).commit()
    private fun cleanupKey(routeId: String) = "$CLEANUP.$routeId"

    override var pending: RelayWakePending?
        get() = read(PENDING)?.let(::decodePending)
        set(value) {
            if (value == null) prefs.edit().remove(PENDING).commit() else save(PENDING, encodePending(value))
        }

    override var active: RelayWakeActive?
        get() = read(ACTIVE_V2)?.let(::decodeActive)
        set(value) {
            if (value == null) prefs.edit().remove(ACTIVE_V2).commit() else save(ACTIVE_V2, encodeActive(value))
        }

    override fun fingerprint(value: String): String = tokenFingerprint(value)

    /**
     * Sticky origin of an upgraded v1 custom route, fixed the first time that route is seen. The value
     * is the canonical origin, or "" when the old origin was never known. It is never re-derived from
     * a later relay choice, so the old credential cannot be queued to a different relay.
     */
    fun legacyBinding(): String? = prefs.getString(LEGACY_BINDING, null)
    fun saveLegacyBinding(origin: String): Boolean = prefs.edit().putString(LEGACY_BINDING, origin).commit()
    fun clearLegacyBinding(): Boolean = prefs.edit().remove(LEGACY_BINDING).commit()

    fun saveEarlyChallenge(challenge: String): Boolean = save(EARLY_CHALLENGE, challenge)
    fun earlyChallenge(): String? = read(EARLY_CHALLENGE)
    fun clearEarlyChallenge(): Boolean = prefs.edit().remove(EARLY_CHALLENGE).commit()

    private fun encodePending(value: RelayWakePending) = (encodeIdentity(value.identity) + listOf(
        value.relayOrigin, value.protocol, value.tokenFingerprint, value.registrationId.orEmpty(),
        value.candidate.routeId, value.candidate.manageCredential, value.candidate.wakeCredential, value.challenge.orEmpty(), value.createdAtMillis.toString(),
    )).joinToString("\n")
    private fun decodePending(value: String) = decodeLines(value, 15) { v ->
        RelayWakePending(decodeIdentity(v), v[6], v[7], v[8], v[9].ifEmpty { null }, RelayRoute(v[10], v[11], v[12]), v[13].ifEmpty { null }, v[14].toLongOrNull() ?: return@decodeLines null)
    }
    private fun encodeActive(value: RelayWakeActive) = (encodeIdentity(value.identity) + listOf(
        value.relayOrigin, value.protocol, value.tokenFingerprint, value.route.routeId,
        value.route.manageCredential, value.route.wakeCredential, value.bearerToken, value.published.toString(),
    )).joinToString("\n")
    private fun decodeActive(value: String) = decodeLines(value, 14) { v ->
        RelayWakeActive(decodeIdentity(v), v[6], v[7], v[8], RelayRoute(v[9], v[10], v[11]), v[12], v[13].toBooleanStrictOrNull() ?: return@decodeLines null,)
    }
    private fun encodeIdentity(value: RelayWakeIdentity) = listOf(value.origin, value.vaultId, value.deviceId, value.provider, value.environment, value.credentialFingerprint)
    private fun decodeIdentity(value: List<String>) = RelayWakeIdentity(value[0], value[1], value[2], value[3], value[4], value[5])
    private fun <T> decodeLines(value: String, size: Int, decode: (List<String>) -> T?): T? = value.split('\n').takeIf { it.size == size }?.let(decode)
    private fun encodeCandidate(candidate: RelayWakeCandidate) = listOf(candidate.registrationId.orEmpty(), candidate.route.routeId, candidate.route.manageCredential, candidate.route.wakeCredential, candidate.tokenFingerprint, candidate.challenge.orEmpty()).joinToString("\n")
    private fun decodeCandidate(value: String): RelayWakeCandidate? = value.split('\n').takeIf { it.size == 6 }?.let { RelayWakeCandidate(it[0].ifEmpty { null }, RelayRoute(it[1], it[2], it[3]), it[4], it[5].ifEmpty { null }) }
    private fun save(key: String, value: String): Boolean = try { prefs.edit().putString(key, box.seal(key, value.toByteArray())).commit() } catch (_: Exception) { false }
    private fun read(key: String): String? = prefs.getString(key, null)?.let { box.open(key, it) }?.toString(Charsets.UTF_8)
    private fun secret() = ByteArray(32).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it) }
    private companion object {
        const val ACTIVE = "hosted-relay-active.v2"; const val CANDIDATE = "hosted-relay-candidate.v2"; const val CLEANUP = "hosted-relay-cleanup.v2"
        const val PENDING = "hosted-relay-pending.v2"; const val ACTIVE_V2 = "hosted-relay-active-binding.v2"; const val EARLY_CHALLENGE = "hosted-relay-early-challenge.v2"
        const val LEGACY_BINDING = "legacy-route-migration-origin.v1"
    }
}
