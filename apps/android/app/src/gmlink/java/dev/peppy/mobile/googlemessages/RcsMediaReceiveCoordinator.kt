package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.GatewayAuthException
import dev.peppy.mobile.attachments.AttachmentRetentionDriver
import dev.peppy.mobile.gateway.GatewayAccountIdentity
import dev.peppy.mobile.attachments.AttachmentStorageLoad
import dev.peppy.mobile.gateway.GatewayAccountHost
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.gateway.GatewayHttp
import dev.peppy.mobile.mms.MmsMediaTransfer
import java.io.File
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeAttachmentState
import uniffi.peppy_mobile_bindings.NativePendingRcsMediaReceive
import uniffi.peppy_mobile_bindings.NativeRcsMediaDownloadAdmission

/** Receives one durable RCS media task; all network I/O runs under the service media queue. */
internal class RcsMediaReceiveCoordinator(
    private val files: RcsMediaPrivateFiles,
    private val queue: RcsMediaTransferQueue,
    private val client: NativeClientInterface,
    private val io: RcsMediaReceiveIo,
    private val current: () -> Boolean,
) {
    constructor(context: Context, files: RcsMediaPrivateFiles, queue: RcsMediaTransferQueue,
        client: NativeClientInterface, linkLease: LinkHandleLease, current: () -> Boolean,
    ) : this(files, queue, client, AndroidRcsMediaReceiveIo(context, linkLease, client), current)

    fun submit(task: NativePendingRcsMediaReceive): Boolean = queue.submit("receive/${task.messageId}/${task.attachmentIndex}") { lease ->
        if (!lease.isCurrent() || !current()) return@submit
        val scope = io.captureAccount() ?: return@submit
        if (!valid(lease, scope) || scopeDenied(scope)) return@submit
        try {
            // Prepared ciphertext must not depend on another capacity lookup (or Google fetch).
            var admission = client.gatewayAdmitRcsMediaDownload(task.messageId, task.attachmentIndex, null)
            if (admission == NativeRcsMediaDownloadAdmission.CapacityUnavailable) {
                val available = capacity(scope) ?: return@submit
                if (!valid(lease, scope)) return@submit
                admission = client.gatewayAdmitRcsMediaDownload(task.messageId, task.attachmentIndex, available)
            }
            when (admission) {
                NativeRcsMediaDownloadAdmission.TooLarge -> mark(lease, task, scope, "too_large")
                NativeRcsMediaDownloadAdmission.QuotaFull -> mark(lease, task, scope, "quota_full")
                NativeRcsMediaDownloadAdmission.CapacityUnavailable -> Unit
                is NativeRcsMediaDownloadAdmission.Held -> downloadAndImport(lease, task, admission.token, scope)
                is NativeRcsMediaDownloadAdmission.ExistingPrepared -> uploadPrepared(lease, task, admission.attachmentId, scope)
            }
        } catch (_: Exception) {
            mark(lease, task, scope, "failed")
        }
    }

    private fun valid(lease: RcsMediaTransferQueue.Lease, scope: GatewayAccountHost.AttachmentStorageScope) =
        lease.isCurrent() && current() && io.isCurrent(scope)

    private fun downloadAndImport(
        lease: RcsMediaTransferQueue.Lease,
        task: NativePendingRcsMediaReceive,
        token: String,
        scope: GatewayAccountHost.AttachmentStorageScope,
    ) {
        val destination = try { files.createDestination() } catch (_: Exception) {
            mark(lease, task, scope, "failed")
            return
        }
        try {
            if (!valid(lease, scope)) return
            val response = io.download(task, destination)
            if (!valid(lease, scope)) return
            if (response == null) {
                mark(lease, task, scope, "failed")
                return
            }
            val code = runCatching { JSONObject(response).optString("code", "failed") }.getOrDefault("failed")
            if (code != "ok") {
                if (code in setOf("auth", "unauthorized", "signed_out")) denyScope(scope)
                else mark(lease, task, scope, if (code in setOf("not_found", "expired")) "expired" else "failed")
                return
            }
            if (!valid(lease, scope)) return
            val attachment = client.gatewayImportRcsMediaDownload(task.messageId, task.attachmentIndex, token, destination.absolutePath)
            uploadPrepared(lease, task, attachment.attachmentId, scope)
        } catch (_: Exception) {
            mark(lease, task, scope, "failed")
        } finally {
            files.delete(destination)
        }
    }

    private fun uploadPrepared(
        lease: RcsMediaTransferQueue.Lease,
        task: NativePendingRcsMediaReceive,
        attachmentId: String,
        scope: GatewayAccountHost.AttachmentStorageScope,
    ) {
        if (!valid(lease, scope)) return
        val attachment = client.attachmentInfo(attachmentId)
        if (attachment.state in setOf(NativeAttachmentState.UPLOADED, NativeAttachmentState.AVAILABLE)) {
            finalize(lease, task, scope)
            return
        }
        if (attachment.state != NativeAttachmentState.PENDING_UPLOAD) {
            mark(lease, task, scope, "failed")
            return
        }
        if (!valid(lease, scope)) return
        val http = fencedHttp(lease, scope) ?: return
        val result = MmsMediaTransfer(
            client = client,
            http = http,
            cacheDirectory = files.rootForTest(),
            pendingUploads = { client.pendingUploads().filter { it.attachmentId == attachmentId } },
            pendingDownloads = { emptyList() },
        ).run(limit = 1)
        if (!valid(lease, scope)) return
        if (client.attachmentInfo(attachmentId).state in setOf(NativeAttachmentState.UPLOADED, NativeAttachmentState.AVAILABLE)) {
            finalize(lease, task, scope)
            return
        }
        when (result.failures.firstOrNull()?.reason) {
            "quota" -> mark(lease, task, scope, "quota_full")
            "auth", "account_paused" -> denyScope(scope)
            else -> mark(lease, task, scope, "failed")
        }
    }

    private fun fencedHttp(lease: RcsMediaTransferQueue.Lease, scope: GatewayAccountHost.AttachmentStorageScope): GatewayHttp? =
        io.http(scope)?.also { http ->
            http.setBeforeRequest { check(valid(lease, scope)) { "RCS media scope retired" } }
        }

    private fun finalize(lease: RcsMediaTransferQueue.Lease, task: NativePendingRcsMediaReceive, scope: GatewayAccountHost.AttachmentStorageScope) {
        var finalized = false
        lease.publish {
            if (!valid(lease, scope)) return@publish
            // Core checks the authoritative hosted descriptor, not this pass's upload count.
            client.gatewayFinalizeRcsMediaDownload(task.messageId, task.attachmentIndex)
            finalized = true
        }
        if (!finalized || !valid(lease, scope)) return
        try {
            val http = fencedHttp(lease, scope)
            if (http != null) {
                val retention = AttachmentRetentionDriver(client, http) { valid(lease, scope) }
                retention.run()
                if (retention.accountPaused) denyScope(scope)
            }
        } catch (_: GatewayAuthException) {
            denyScope(scope)
        } catch (_: Exception) {
            // The gateway worker retries retention before publishing the queued descriptor.
        }
        lease.publish { if (valid(lease, scope)) io.wake() }
    }

    /** One bounded shared slot coalesces even unavailable status across a serial receive pass.
     * This IO-only advisory use is separate from the settings UI; it never reserves hosted bytes.
     */
    private fun capacity(scope: GatewayAccountHost.AttachmentStorageScope): ULong? {
        val cached = synchronized(capacityLock) {
            capacityCache?.takeIf { it.identity == scope.identity && it.expiresAtMs > io.nowMs() }
        }
        val storage = if (cached != null) cached.storage else {
            if (!current() || !io.isCurrent(scope)) return null
            // Queue serialization coalesces reads; never hold the lifecycle cache lock over IO.
            io.capacity(scope).also {
                if (current() && io.isCurrent(scope)) synchronized(capacityLock) {
                    capacityCache = CachedCapacity(scope.identity, it, io.nowMs() + CAPACITY_CACHE_MS)
                }
            }
        }
        val status = (storage as? AttachmentStorageLoad.Available)?.status ?: return null
        if (status.usedBytes < 0 || status.limitBytes < 0) return null
        return (status.limitBytes - status.usedBytes).coerceAtLeast(0).toULong()
    }

    private fun scopeDenied(scope: GatewayAccountHost.AttachmentStorageScope): Boolean = synchronized(capacityLock) {
        capacityCache?.let { it.identity == scope.identity && it.expiresAtMs > io.nowMs() && it.storage == AttachmentStorageLoad.AuthorizationFailure } == true
    }

    private fun denyScope(scope: GatewayAccountHost.AttachmentStorageScope) = synchronized(capacityLock) {
        capacityCache = CachedCapacity(scope.identity, AttachmentStorageLoad.AuthorizationFailure, io.nowMs() + CAPACITY_CACHE_MS)
    }

    private fun mark(lease: RcsMediaTransferQueue.Lease, task: NativePendingRcsMediaReceive, scope: GatewayAccountHost.AttachmentStorageScope, state: String) {
        lease.publish {
            if (valid(lease, scope)) {
                client.gatewayMarkRcsMediaDownloadState(task.messageId, task.attachmentIndex, state)
                if (valid(lease, scope)) io.wake()
            }
        }
    }

    private data class CachedCapacity(val identity: GatewayAccountIdentity, val storage: AttachmentStorageLoad?, val expiresAtMs: Long)

    internal companion object {
        private const val CAPACITY_CACHE_MS = 5_000L
        private val capacityLock = Any()
        private var capacityCache: CachedCapacity? = null
        fun clearCapacityCache() = synchronized(capacityLock) { capacityCache = null }
    }
}

/** Native host effects; core admission and hosted transfer remain in the coordinator. */
internal interface RcsMediaReceiveIo {
    fun captureAccount(): GatewayAccountHost.AttachmentStorageScope?
    fun isCurrent(scope: GatewayAccountHost.AttachmentStorageScope): Boolean
    fun capacity(scope: GatewayAccountHost.AttachmentStorageScope): AttachmentStorageLoad?
    fun http(scope: GatewayAccountHost.AttachmentStorageScope): GatewayHttp?
    fun download(task: NativePendingRcsMediaReceive, destination: File): String?
    fun wake()
    fun nowMs(): Long = System.nanoTime() / 1_000_000
}

private class AndroidRcsMediaReceiveIo(
    private val context: Context,
    private val linkLease: LinkHandleLease,
    private val client: NativeClientInterface,
) : RcsMediaReceiveIo {
    override fun captureAccount() = GatewayAccountHost.attachmentStorageScope(context)
    override fun isCurrent(scope: GatewayAccountHost.AttachmentStorageScope) =
        NativeGateway.isCurrentAccountIdentity(context, scope.identity) && NativeGateway.open(context) === client
    override fun capacity(scope: GatewayAccountHost.AttachmentStorageScope) = GatewayAccountHost.loadAttachmentStorage(context, scope)
    override fun http(scope: GatewayAccountHost.AttachmentStorageScope) = NativeGateway.currentAccountHttp(context, scope.identity)
    override fun download(task: NativePendingRcsMediaReceive, destination: File) = LinkRuntime.withCapturedLinkOperation(linkLease) { handle ->
        handle.downloadRcsMedia(JSONObject().put("media_ref", task.mediaRef).put("dest_path", destination.absolutePath)
            .put("max_bytes", task.plaintextBytes.toLong()).toString())
    }
    override fun wake() = GatewayWork.enqueueAfterCapture(context)
}
