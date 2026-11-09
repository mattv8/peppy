package dev.peppy.mobile.relay

import android.content.SharedPreferences
import dev.peppy.mobile.RelayRoute
import dev.peppy.mobile.security.KeystoreSecretBox
import dev.peppy.mobile.security.SecretBox
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.ArrayDeque
import java.util.UUID
import java.util.WeakHashMap
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

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

internal class RelayWakeStateUnavailableException : IllegalStateException("relay wake protected state is unavailable")

/** Keystore-sealed atomic slots; commit before every network effect. */
internal class RelayWakeStateStore(private val prefs: SharedPreferences, private val box: SecretBox = KeystoreSecretBox) : RelayWakeCoordinatorStore {
    fun tokenFingerprint(token: String): String = MessageDigest.getInstance("SHA-256").digest(token.toByteArray()).joinToString("") { "%02x".format(it) }
    fun newCandidate(token: String) = RelayWakeCandidate(null, RelayRoute(UUID.randomUUID().toString(), secret(), secret()), tokenFingerprint(token))
    fun saveCandidate(candidate: RelayWakeCandidate): Boolean = save(CANDIDATE, encodeCandidate(candidate))
    fun candidate(): RelayWakeCandidate? = read(CANDIDATE)?.let { decodeCandidate(it) ?: unavailable() }
    fun clearCandidate(): Boolean = remove(CANDIDATE)
    fun saveActive(route: RelayRoute, fingerprint: String): Boolean = save(ACTIVE, "$fingerprint\n${route.routeId}\n${route.manageCredential}\n${route.wakeCredential}")
    fun active(): Pair<String, RelayRoute>? = read(ACTIVE)?.let { decodeLegacyActive(it) ?: unavailable() }
    fun clearActive(): Boolean = remove(ACTIVE)
    /** Cleanup records are route-keyed so a new rotation cannot erase an older revocation. */
    fun saveCleanup(cleanup: RelayWakeCleanup): Boolean = save(cleanupKey(cleanup.route.routeId), listOf(cleanup.identity.origin, cleanup.identity.vaultId, cleanup.identity.deviceId, cleanup.identity.provider, cleanup.identity.environment, cleanup.identity.credentialFingerprint, cleanup.route.routeId, cleanup.route.manageCredential, cleanup.route.wakeCredential, cleanup.bearerToken, cleanup.relayOrigin).joinToString("\n"))
    fun cleanups(): List<RelayWakeCleanup> {
        if (!recoverAcknowledgedState()) unavailable()
        return prefs.all.keys.filter { it.startsWith("$CLEANUP.") }.map { key ->
            read(key)?.let(::decodeCleanup) ?: unavailable()
        }
    }
    fun cleanup(): RelayWakeCleanup? = cleanups().firstOrNull()
    fun clearCleanup(): Boolean {
        if (!recoverAcknowledgedState()) return false
        return cleanup()?.let { clearCleanup(it.route.routeId) } ?: true
    }
    fun clearCleanup(routeId: String): Boolean = remove(cleanupKey(routeId))
    private fun cleanupKey(routeId: String) = "$CLEANUP.$routeId"

    override var pending: RelayWakePending?
        get() = read(PENDING)?.let { decodePending(it) ?: unavailable() }
        set(value) {
            savePending(value)
        }

    override var active: RelayWakeActive?
        get() = read(ACTIVE_V2)?.let { decodeActive(it) ?: unavailable() }
        set(value) {
            saveActive(value)
        }

    override fun savePending(value: RelayWakePending?): Boolean = if (value == null) remove(PENDING) else save(PENDING, encodePending(value))
    override fun saveActive(value: RelayWakeActive?): Boolean = if (value == null) remove(ACTIVE_V2) else save(ACTIVE_V2, encodeActive(value))
    override fun acknowledgePublished(expected: RelayWakeActive): RelayWakePublicationAck {
        val journal = journal()
        return journal.lock.withLock {
            if (!recoverLocked(journal)) return@withLock RelayWakePublicationAck.RETRY
            if (readAcknowledged(ACTIVE_V2) != encodeActive(expected)) return@withLock RelayWakePublicationAck.STALE
            val published = RelayWakeActive(expected.identity, expected.relayOrigin, expected.protocol, expected.tokenFingerprint, expected.route, expected.bearerToken, true)
            val mutation = try {
                Mutation(mapOf(ACTIVE_V2 to box.seal(ACTIVE_V2, encodeActive(published).toByteArray())), emptySet())
            } catch (_: Exception) {
                return@withLock RelayWakePublicationAck.RETRY
            }
            if (commit(mutation)) RelayWakePublicationAck.ACKNOWLEDGED else {
                journal.pending.addLast(mutation)
                RelayWakePublicationAck.RETRY
            }
        }
    }
    override fun promote(pending: RelayWakePending, active: RelayWakeActive): Boolean = try {
        mutate(
            mapOf(ACTIVE_V2 to box.seal(ACTIVE_V2, encodeActive(active).toByteArray())),
            setOf(PENDING),
            expectedPending = encodePending(pending),
        )
    } catch (_: Exception) { false }

    override fun recoverAcknowledgedState(): Boolean {
        val journal = journal()
        return journal.lock.withLock { recoverLocked(journal) }
    }
    override fun <T> runAcknowledgedEffect(effect: () -> T): T? {
        val journal = journal()
        return journal.lock.withLock { if (recoverLocked(journal)) effect() else null }
    }
    override fun fingerprint(value: String): String = tokenFingerprint(value)

    /**
     * Sticky origin of an upgraded v1 custom route, fixed the first time that route is seen. The value
     * is the canonical origin, or "" when the old origin was never known. It is never re-derived from
     * a later relay choice, so the old credential cannot be queued to a different relay.
     */
    fun legacyBinding(): String? = if (recoverAcknowledgedState()) prefs.getString(LEGACY_BINDING, null) else null
    fun saveLegacyBinding(origin: String): Boolean = mutate(mapOf(LEGACY_BINDING to origin))
    fun clearLegacyBinding(): Boolean = remove(LEGACY_BINDING)

    fun saveEarlyChallenge(challenge: String): Boolean = save(EARLY_CHALLENGE, challenge)
    fun earlyChallenge(): String? = read(EARLY_CHALLENGE)?.takeIf(RelayWakeCoordinator.SECRET_RE::matches) ?:
        if (prefs.contains(EARLY_CHALLENGE)) unavailable() else null
    fun clearEarlyChallenge(): Boolean = remove(EARLY_CHALLENGE)

    private fun decodeLegacyActive(value: String): Pair<String, RelayRoute>? = value.split('\n').takeIf { it.size == 4 }?.let {
        it[0] to RelayRoute(it[1], it[2], it[3])
    }
    private fun decodeCleanup(value: String): RelayWakeCleanup? = value.split('\n').takeIf { it.size == 11 }?.let { values ->
        RelayWakeCleanup(
            RelayWakeIdentity(values[0], values[1], values[2], values[3], values[4], values[5]),
            RelayRoute(values[6], values[7], values[8]), values[9], values[10],
        )
    }

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
    private fun save(key: String, value: String): Boolean = try {
        mutate(mapOf(key to box.seal(key, value.toByteArray())))
    } catch (_: Exception) { false }
    private fun remove(key: String): Boolean = mutate(removals = setOf(key))
    private fun read(key: String): String? {
        if (!recoverAcknowledgedState()) unavailable()
        val sealed = prefs.getString(key, null) ?: return null
        return box.open(key, sealed)?.toString(Charsets.UTF_8) ?: unavailable()
    }

    private fun mutate(
        puts: Map<String, String> = emptyMap(),
        removals: Set<String> = emptySet(),
        expectedPending: String? = null,
    ): Boolean {
        val journal = journal()
        return journal.lock.withLock {
            if (!recoverLocked(journal)) return@withLock false
            if (expectedPending != null && readAcknowledged(PENDING) != expectedPending) return@withLock false
            val mutation = Mutation(puts, removals)
            if (commit(mutation)) true else { journal.pending.addLast(mutation); false }
        }
    }

    private fun recoverLocked(journal: Journal): Boolean {
        while (journal.pending.isNotEmpty()) {
            if (!commit(journal.pending.first())) return false
            journal.pending.removeFirst()
        }
        return protectedSlotsReadable()
    }

    private fun protectedSlotsReadable(): Boolean = try {
        prefs.all.entries.filter { isProtectedKey(it.key) }.all { (key, stored) ->
            val sealed = stored as? String ?: return@all false
            val value = box.open(key, sealed)?.toString(Charsets.UTF_8) ?: return@all false
            when {
                key == CANDIDATE -> decodeCandidate(value) != null
                key == ACTIVE -> decodeLegacyActive(value) != null
                key.startsWith("$CLEANUP.") -> decodeCleanup(value) != null
                key == PENDING -> decodePending(value) != null
                key == ACTIVE_V2 -> decodeActive(value) != null
                key == EARLY_CHALLENGE -> RelayWakeCoordinator.SECRET_RE.matches(value)
                else -> true
            }
        }
    } catch (_: Exception) { false }

    private fun isProtectedKey(key: String) = key == CANDIDATE || key == ACTIVE || key.startsWith("$CLEANUP.") ||
        key == PENDING || key == ACTIVE_V2 || key == EARLY_CHALLENGE

    private fun unavailable(): Nothing = throw RelayWakeStateUnavailableException()

    private fun readAcknowledged(key: String): String? = prefs.getString(key, null)?.let { box.open(key, it) }?.toString(Charsets.UTF_8)

    private fun journal(): Journal = synchronized(journalsLock) { journals.getOrPut(prefs, ::Journal) }

    private fun commit(mutation: Mutation): Boolean = try {
        val editor = prefs.edit()
        mutation.removals.forEach { key -> editor.remove(key) }
        mutation.puts.forEach { (key, value) -> editor.putString(key, value) }
        editor.commit()
    } catch (_: Exception) { false }
    private fun secret() = ByteArray(32).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it) }
    private data class Mutation(val puts: Map<String, String>, val removals: Set<String>)
    private class Journal(val lock: ReentrantLock = ReentrantLock(), val pending: ArrayDeque<Mutation> = ArrayDeque())

    private companion object {
        val journalsLock = Any()
        val journals = WeakHashMap<SharedPreferences, Journal>()
        const val ACTIVE = "hosted-relay-active.v2"; const val CANDIDATE = "hosted-relay-candidate.v2"; const val CLEANUP = "hosted-relay-cleanup.v2"
        const val PENDING = "hosted-relay-pending.v2"; const val ACTIVE_V2 = "hosted-relay-active-binding.v2"; const val EARLY_CHALLENGE = "hosted-relay-early-challenge.v2"
        const val LEGACY_BINDING = "legacy-route-migration-origin.v1"
    }
}
