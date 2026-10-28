package dev.peppy.mobile

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.NativeClient

internal data class OwnerComputerRequest(val origin: String, val invitationId: String, val promptDigest: String, val requestJson: String)
internal data class OwnerComputerView(val origin: String, val invitationId: String, val promptDigest: String, val created: Boolean, val sas: String?)

internal interface OwnerComputerClient : AutoCloseable {
    fun createRequest(qrJson: String): OwnerComputerRequest
    fun consumeCreated(invitationId: String, promptDigest: String, createdJson: String)
    fun view(): OwnerComputerView
    fun approve(invitationId: String, promptDigest: String, vaultKeyPhrase: CharArray, nativeClient: NativeClient): String
    fun cancel()
}

/** Owner approval transport. The QR and vault key phrase remain in the binding and secure store. */
internal class OwnerComputerApprovalHost(
    private val client: OwnerComputerClient,
    private val transport: OwnerPhoneTransport,
    private val secrets: VaultSecretStore,
    private val vaultId: String,
    private val deviceId: String,
    private val nativeClient: () -> NativeClient?,
    private val expectedOrigin: String,
    private val hosted: Boolean,
    private val accountSession: String?,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) {
    sealed interface Result {
        data class View(val view: OwnerComputerView) : Result
        /** Approval was accepted by the server; it must never be cancelled locally or remotely. */
        data class Approved(val view: OwnerComputerView) : Result
        data object Cancelled : Result
        data class Failed(val code: String) : Result
    }

    private val gate = Mutex()
    private var active: OwnerComputerView? = null
    private var approved = false

    fun close() = client.close()
    suspend fun scan(qrJson: String): Result = serialized {
        try {
            val request = client.createRequest(qrJson)
            if (request.origin != expectedOrigin) return@serialized failAndClear("invalid-qr")
            val created = request("POST", COMPUTER_PATH, request.requestJson) ?: return@serialized Result.Failed("offline")
            if (created.status !in 200..299 || created.body == null) return@serialized failAndClear(responseFailure(created.status))
            client.consumeCreated(request.invitationId, request.promptDigest, created.body)
            val view = client.view()
            if (!view.created || view.sas.isNullOrBlank() || view.invitationId != request.invitationId || view.promptDigest != request.promptDigest) {
                return@serialized failAndClear("invalid-qr")
            }
            active = view
            approved = false
            Result.View(view)
        } catch (_: Exception) { failAndClear("invalid-qr") }
    }

    suspend fun approve(codesMatch: Boolean, prompt: SecretPrompt): Result = serialized {
        if (approved) return@serialized active?.let(Result::Approved) ?: Result.Failed("enrollment-state")
        if (!codesMatch) return@serialized Result.Failed("codes-do-not-match")
        val initial = active ?: return@serialized Result.Failed("enrollment-state")
        val phrase = try {
            when (val stored = secrets.read(vaultId, deviceId)) {
                is StoredSecret.Present -> stored.value
                StoredSecret.Missing -> prompt.ask() ?: return@serialized Result.Cancelled
            }
        } catch (_: SecureStoreUnavailable) {
            return@serialized Result.Failed("secure-store-unavailable")
        }
        try {
            val current = client.view()
            if (current.invitationId != initial.invitationId || current.promptDigest != initial.promptDigest || !current.created) {
                return@serialized Result.Cancelled
            }
            val core = nativeClient() ?: return@serialized Result.Failed("lock-required")
            val approval = client.approve(current.invitationId, current.promptDigest, phrase, core)
            val response = request("POST", "$COMPUTER_PATH/${current.invitationId}/approve", approval)
                ?: return@serialized Result.Failed("offline")
            if (response.status !in 200..299) return@serialized Result.Failed(responseFailure(response.status))
            approved = true
            Result.Approved(current)
        } catch (_: Exception) {
            Result.Failed("enrollment")
        } finally {
            phrase.fill('\u0000')
        }
    }

    suspend fun cancel(): Result = serialized {
        val current = active
        if (approved) return@serialized current?.let(Result::Approved) ?: Result.Failed("enrollment-state")
        try { client.cancel() } catch (_: Exception) { return@serialized Result.Failed("enrollment") }
        active = null
        if (current == null) return@serialized Result.Cancelled
        val response = request("DELETE", "$COMPUTER_PATH/${current.invitationId}", null) ?: return@serialized Result.Failed("offline")
        if (response.status !in 200..299 && response.status !in setOf(404, 410)) return@serialized Result.Failed(responseFailure(response.status))
        Result.Cancelled
    }

    private suspend fun <T> serialized(block: suspend () -> T): T = gate.withLock { withContext(ioDispatcher) { block() } }
    private fun request(method: String, path: String, body: String?): EnrollmentHttpResponse? {
        if (hosted && accountSession == null) return null
        return transport.request(method, path, body, accountSession)
    }
    private fun failAndClear(code: String): Result.Failed {
        try { client.cancel() } catch (_: Exception) { }
        active = null
        approved = false
        return Result.Failed(code)
    }
    private fun responseFailure(status: Int) = when (status) { 404, 410 -> "expired"; 401, 403 -> "server-rejected"; else -> "server-rejected" }
    private companion object { const val COMPUTER_PATH = "/v1/enrollment/computers" }
}
