package dev.peppy.mobile

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

internal interface OwnerPhoneClient : AutoCloseable {
    fun createInvitationRequest(method: OwnerPhoneMethod): String
    fun consumeCreated(createdJson: String)
    fun qrPayload(): String
    fun status(statusJson: String): OwnerPhoneStatus
    fun invitationId(): String?
    fun method(): OwnerPhoneMethod?
    fun secretMatches(secret: CharArray): Boolean
    fun approveRequest(statusJson: String, secret: CharArray): String
}

internal enum class OwnerPhoneMethod { TRANSFER, SHARED_SECRET }
internal enum class OwnerPhoneStage { WAITING, VERIFY, APPROVED, CONNECTED, EXPIRED, DENIED }
internal data class OwnerPhoneStatus(
    val stage: OwnerPhoneStage,
    val expiresInSeconds: Int,
    val sas: String?,
    val claimDigest: String?,
    val invitationId: String,
)
internal fun interface OwnerPhoneTransport {
    fun request(method: String, path: String, body: String?, accountSession: String?): EnrollmentHttpResponse?
}
internal fun interface SecretPrompt { suspend fun ask(): CharArray? }

/**
 * Owner-side phone approval. Every public operation runs under one [Mutex] on [ioDispatcher], so blocking
 * HTTP and Argon2 checks never touch the main thread and status snapshots never interleave.
 */
internal class OwnerPhoneApprovalHost(
    private val client: OwnerPhoneClient,
    private val transport: OwnerPhoneTransport,
    private val secrets: VaultSecretStore,
    private val vaultId: String,
    private val deviceId: String,
    private val shouldRotate: (OwnerPhoneStage, Int, OwnerPhoneMethod, OwnerPhoneMethod) -> Boolean,
    private val hosted: Boolean = false,
    private val accountSession: String? = null,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) {
    sealed interface Result {
        data class View(val status: OwnerPhoneStatus, val qr: String?) : Result
        data object Cancelled : Result
        data class Failed(val code: String) : Result
    }

    private val gate = Mutex()
    private var latest: OwnerPhoneStatus? = null
    private var latestStatusJson: String? = null
    private var approvedInvitationId: String? = null
    private var pinnedClaimDigest: String? = null

    fun close() = client.close()

    suspend fun start(method: OwnerPhoneMethod): Result = serialized { startLocked(method) }

    suspend fun poll(): Result = serialized { pollLocked() }

    suspend fun rotateIfNeeded(requested: OwnerPhoneMethod): Result = serialized {
        val result = pollLocked()
        val status = (result as? Result.View)?.status ?: return@serialized result
        if (status.stage == OwnerPhoneStage.APPROVED) return@serialized result
        val active = client.method() ?: return@serialized Result.Failed("enrollment-state")
        if (!shouldRotate(status.stage, status.expiresInSeconds, active, requested)) return@serialized result
        replaceInvitationLocked(requested)
    }

    /** Cancels the current invitation, then creates a new one. Used for restart after a terminal stage. */
    suspend fun restart(method: OwnerPhoneMethod): Result = serialized { replaceInvitationLocked(method) }

    suspend fun approve(prompt: SecretPrompt): Result = serialized { approveLocked(prompt) }

    suspend fun cancel(): Result = serialized { cancelLocked() }

    private suspend fun <T> serialized(block: suspend () -> T): T = gate.withLock {
        withContext(ioDispatcher) { block() }
    }

    private fun startLocked(method: OwnerPhoneMethod): Result {
        val created = request("POST", "/v1/enrollment/invitations", client.createInvitationRequest(method))
            ?: return accountRequired()
        if (created.status !in 200..299) return failed(created.status)
        val body = created.body ?: return Result.Failed("server-rejected")
        return try {
            client.consumeCreated(body)
            pinnedClaimDigest = null
            pollLocked()
        } catch (_: Exception) {
            Result.Failed("enrollment")
        }
    }

    private fun pollLocked(): Result {
        val id = client.invitationId() ?: return Result.Failed("enrollment-state")
        val response = request("GET", invitationPath(id), null) ?: return accountRequired()
        if (response.status !in 200..299) return failed(response.status)
        val body = response.body ?: return Result.Failed("server-rejected")
        return try {
            view(client.status(body), body)
        } catch (_: ClaimChangedException) {
            Result.Failed("expired")
        } catch (_: Exception) {
            Result.Failed("enrollment")
        }
    }

    private fun replaceInvitationLocked(method: OwnerPhoneMethod): Result {
        val cancelled = cancelLocked()
        return if (cancelled is Result.Cancelled) startLocked(method) else cancelled
    }

    private suspend fun approveLocked(prompt: SecretPrompt): Result {
        val initial = pollLocked()
        val status = (initial as? Result.View)?.status ?: return initial
        if (status.stage != OwnerPhoneStage.VERIFY) return Result.Failed("enrollment-state")
        if (client.method() == OwnerPhoneMethod.SHARED_SECRET) return submit(status, CharArray(0))

        return try {
            when (val stored = secrets.read(vaultId, deviceId)) {
                StoredSecret.Missing -> approvePrompted(status, prompt)
                is StoredSecret.Present -> try {
                    if (!client.secretMatches(stored.value)) Result.Failed("vault-secret-mismatch") else submit(status, stored.value)
                } finally { stored.value.fill('\u0000') }
            }
        } catch (_: SecureStoreUnavailable) {
            Result.Failed("secure-store-unavailable")
        }
    }

    private fun cancelLocked(): Result {
        val status = latest ?: return Result.Cancelled
        if (status.stage != OwnerPhoneStage.WAITING && status.stage != OwnerPhoneStage.EXPIRED) return Result.Cancelled
        val response = request("DELETE", invitationPath(status.invitationId), null) ?: return accountRequired()
        return when (response.status) {
            in 200..299, 404, 410 -> {
                pinnedClaimDigest = null
                Result.Cancelled
            }
            else -> failed(response.status)
        }
    }

    private suspend fun approvePrompted(initial: OwnerPhoneStatus, prompt: SecretPrompt): Result {
        val secret = prompt.ask() ?: return Result.Cancelled
        try {
            val refreshed = pollLocked()
            val status = (refreshed as? Result.View)?.status ?: return refreshed
            if (status.invitationId != initial.invitationId || status.claimDigest != initial.claimDigest) return Result.Cancelled
            if (!client.secretMatches(secret)) return Result.Failed("wrong-passphrase")
            secrets.saveIfMissing(vaultId, deviceId, secret)
            return submit(status, secret)
        } finally {
            secret.fill('\u0000')
        }
    }

    private fun submit(status: OwnerPhoneStatus, secret: CharArray): Result {
        return try {
            val refreshed = pollLocked()
            val current = (refreshed as? Result.View)?.status ?: return refreshed
            if (current.invitationId != status.invitationId || current.claimDigest != status.claimDigest) return Result.Cancelled
            val statusJson = latestStatusJson ?: return Result.Failed("enrollment-state")
            val response = request("POST", "${invitationPath(current.invitationId)}/approve", client.approveRequest(statusJson, secret))
                ?: return accountRequired()
            if (response.status !in 200..299) return failed(response.status)
            approvedInvitationId = current.invitationId
            val body = response.body
            if (body == null) view(current.copy(stage = OwnerPhoneStage.APPROVED), statusJson)
            else view(normalized(client.status(body)), body)
        } catch (_: Exception) {
            Result.Failed("enrollment")
        } finally {
            secret.fill('\u0000')
        }
    }

    /** An approved invitation remains approved until its recipient reports that it connected. */
    private fun normalized(status: OwnerPhoneStatus): OwnerPhoneStatus =
        if (status.invitationId == approvedInvitationId &&
            (status.stage == OwnerPhoneStage.EXPIRED || status.stage == OwnerPhoneStage.VERIFY)
        ) status.copy(stage = OwnerPhoneStage.APPROVED) else status

    private fun view(status: OwnerPhoneStatus, statusJson: String): Result.View {
        val digest = status.claimDigest
        if (digest != null) {
            if (status.invitationId != client.invitationId() ||
                (pinnedClaimDigest != null && pinnedClaimDigest != digest)
            ) throw ClaimChangedException()
            pinnedClaimDigest = digest
        }
        val normalized = normalized(status)
        latest = normalized
        latestStatusJson = statusJson
        return Result.View(normalized, if (normalized.stage == OwnerPhoneStage.WAITING) client.qrPayload() else null)
    }

    private class ClaimChangedException : IllegalStateException()

    private fun request(method: String, path: String, body: String?): EnrollmentHttpResponse? {
        if (hosted && accountSession == null) return null
        return transport.request(method, path, body, accountSession)
    }

    private fun accountRequired(): Result = if (hosted && accountSession == null) Result.Failed("account-required") else Result.Failed("offline")
    private fun failed(status: Int): Result.Failed = Result.Failed(when (status) {
        404, 410 -> "expired"
        409 -> "enrollment-state"
        else -> "server-rejected"
    })
    private fun invitationPath(id: String) = "/v1/enrollment/invitations/$id"
}
