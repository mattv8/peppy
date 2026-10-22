package dev.peppy.mobile

import org.json.JSONObject
import java.io.IOException
import java.security.SecureRandom
import java.util.UUID

/** Persisted enrollment state is identity-bound; its credentials must never cross the UI boundary. */
internal class RelayWakePending(
    val identity: RelayWakeIdentity,
    val relayOrigin: String,
    val protocol: String,
    val tokenFingerprint: String,
    val registrationId: String?,
    val candidate: RelayRoute,
    val challenge: String?,
    val createdAtMillis: Long,
) {
    override fun toString() = "RelayWakePending(redacted)"
}

internal class RelayWakeActive(
    val identity: RelayWakeIdentity,
    val relayOrigin: String,
    val protocol: String,
    val tokenFingerprint: String,
    val route: RelayRoute,
    val bearerToken: String,
    val published: Boolean,
) {
    override fun toString() = "RelayWakeActive(redacted)"
}

internal interface RelayWakeCoordinatorStore {
    var pending: RelayWakePending?
    var active: RelayWakeActive?
    fun fingerprint(value: String): String
}

internal enum class RelayWakeReconcile { PENDING, REGISTERED, RETRY }

/**
 * The single lifecycle implementation used by the worker. Every state transition commits through
 * [RelayWakeCoordinatorStore] before its corresponding network effect.
 */
internal class RelayWakeCoordinator(
    private val state: RelayWakeCoordinatorStore,
    private val transport: RelayWakeTransport,
    private val publisher: RelayRoutePublisher,
    private val retire: (RelayWakeActive) -> Boolean = { true },
    private val retirePending: (RelayWakePending) -> Boolean = { true },
    private val publishActive: (RelayWakeActive) -> Unit = { active -> publisher.publish(active.route.routeId, active.route.wakeCredential) },
    private val nowMillis: () -> Long = System::currentTimeMillis,
) {
    fun reconcile(
        identity: RelayWakeIdentity,
        relayOrigin: String,
        protocol: String,
        deviceToken: String,
        tokenFingerprint: String,
        bearerToken: String,
        supportsCandidates: Boolean,
    ): RelayWakeReconcile {
        val active = state.active
        if (active != null && active.identity.matches(identity) && active.relayOrigin == relayOrigin &&
            active.protocol == protocol && active.tokenFingerprint == tokenFingerprint
        ) {
            if (active.published) return RelayWakeReconcile.REGISTERED
            return publish(active, identity)
        }

        if (active != null) {
            // Rotate only after the old route has entered the durable cleanup queue.
            if (!retire(active)) return RelayWakeReconcile.RETRY
            state.active = null
        }
        val pending = state.pending
        if (pending != null && (!pending.identity.matches(identity) || pending.relayOrigin != relayOrigin ||
                pending.protocol != protocol || pending.tokenFingerprint != tokenFingerprint)
        ) {
            // A changed binding can never resume another identity's proof.
            if (pending.registrationId != null && pending.challenge != null && !retirePending(pending)) return RelayWakeReconcile.RETRY
            state.pending = null
        }

        val current = state.pending
        if (current != null) {
            if (current.registrationId != null && current.challenge != null) {
                return confirm(current, identity, bearerToken, supportsCandidates)
            }
            if (current.registrationId != null && nowMillis() - current.createdAtMillis >= CHALLENGE_TTL_MILLIS) {
                if (current.challenge != null && !retirePending(current)) return RelayWakeReconcile.RETRY
                state.pending = null
                return reconcile(identity, relayOrigin, protocol, deviceToken, tokenFingerprint, bearerToken, supportsCandidates)
            }
            if (current.registrationId == null) return register(current, deviceToken, identity, bearerToken, supportsCandidates)
            // A challenge received before the registration response stays in the pending record.
            // It is consumed on a later serialized reconcile, never by the FCM callback.
            return RelayWakeReconcile.PENDING
        }

        val candidate = RelayRoute(UUID.randomUUID().toString(), secret(), secret())
        val created = RelayWakePending(identity, relayOrigin, protocol, tokenFingerprint, null, candidate, null, nowMillis())
        state.pending = created
        return register(created, deviceToken, identity, bearerToken, supportsCandidates)
    }

    private fun register(pending: RelayWakePending, deviceToken: String, identity: RelayWakeIdentity, bearerToken: String, supportsCandidates: Boolean): RelayWakeReconcile {
        val response = try {
            transport.post("/v1/registrations", JSONObject().put("provider", "fcm").put("device_token", deviceToken).toString())
        } catch (_: IOException) {
            return RelayWakeReconcile.RETRY
        }
        if (response.code != 201) return RelayWakeReconcile.RETRY
        val registrationId = try { JSONObject(requireNotNull(response.body)).getString("registration_id") } catch (_: Exception) { return RelayWakeReconcile.RETRY }
        if (!UUID_RE.matches(registrationId)) return RelayWakeReconcile.RETRY
        // The callback can save a challenge while the registration request is in flight. Read the
        // committed record rather than the stale local object before attaching the returned id.
        val earlyChallenge = state.pending?.takeIf {
            it.identity.matches(identity) && it.relayOrigin == pending.relayOrigin && it.tokenFingerprint == pending.tokenFingerprint
        }?.challenge
        val registered = RelayWakePending(identity, pending.relayOrigin, pending.protocol, pending.tokenFingerprint, registrationId, pending.candidate, earlyChallenge, nowMillis())
        state.pending = registered
        return if (registered.challenge != null) confirm(registered, identity, bearerToken, supportsCandidates) else RelayWakeReconcile.PENDING
    }

    fun saveChallenge(challenge: String) {
        if (!SECRET_RE.matches(challenge)) return
        val pending = state.pending?.takeIf { it.registrationId != null } ?: return
        state.pending = RelayWakePending(pending.identity, pending.relayOrigin, pending.protocol, pending.tokenFingerprint,
            pending.registrationId, pending.candidate, challenge, pending.createdAtMillis)
    }

    private fun confirm(pending: RelayWakePending, identity: RelayWakeIdentity, bearerToken: String, supportsCandidates: Boolean): RelayWakeReconcile {
        val registrationId = pending.registrationId ?: return RelayWakeReconcile.PENDING
        val challenge = pending.challenge ?: return RelayWakeReconcile.PENDING
        val body = JSONObject().put("challenge", challenge).also { request ->
            if (supportsCandidates) request.put("route_id", pending.candidate.routeId)
                .put("manage_credential", pending.candidate.manageCredential).put("wake_credential", pending.candidate.wakeCredential)
        }
        val response = try { transport.post("/v1/registrations/$registrationId/confirm", body.toString()) } catch (_: IOException) { return RelayWakeReconcile.RETRY }
        if (response.code != 201) {
            if (response.code == 408 || response.code == 429 || response.code !in 400..499) return RelayWakeReconcile.RETRY
            state.pending = null
            return RelayWakeReconcile.RETRY
        }
        val route = try {
            val responseBody = JSONObject(requireNotNull(response.body))
            RelayRoute(responseBody.getString("route_id"), responseBody.getString("manage_credential"), responseBody.getString("wake_credential"))
        } catch (_: Exception) { return RelayWakeReconcile.RETRY }
        if (supportsCandidates && !sameRoute(route, pending.candidate)) return RelayWakeReconcile.RETRY
        val active = RelayWakeActive(identity, pending.relayOrigin, pending.protocol, pending.tokenFingerprint, route, bearerToken, false)
        state.active = active
        state.pending = null
        return publish(active, identity)
    }

    private fun publish(active: RelayWakeActive, identity: RelayWakeIdentity): RelayWakeReconcile {
        if (!active.identity.matches(identity)) return RelayWakeReconcile.PENDING
        return try {
            publishActive(active)
            // A pause/rotation can remove this route while PUT is in flight. Never resurrect it.
            val current = state.active
            if (current == null || current.route.routeId != active.route.routeId || !current.identity.matches(active.identity)) {
                retire(active)
                return RelayWakeReconcile.RETRY
            }
            state.active = RelayWakeActive(active.identity, active.relayOrigin, active.protocol, active.tokenFingerprint, active.route, active.bearerToken, true)
            RelayWakeReconcile.REGISTERED
        } catch (_: IOException) {
            RelayWakeReconcile.RETRY
        }
    }

    private fun sameRoute(left: RelayRoute, right: RelayRoute) = left.routeId == right.routeId &&
        left.manageCredential == right.manageCredential && left.wakeCredential == right.wakeCredential

    private fun secret() = ByteArray(32).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it) }

    companion object {
        val UUID_RE = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", RegexOption.IGNORE_CASE)
        val SECRET_RE = Regex("[0-9a-f]{64}", RegexOption.IGNORE_CASE)
        const val CHALLENGE_TTL_MILLIS = 10 * 60 * 1000L
    }
}
