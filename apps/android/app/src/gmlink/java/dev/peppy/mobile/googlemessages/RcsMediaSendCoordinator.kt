package dev.peppy.mobile.googlemessages

import java.io.File
import org.json.JSONObject

/** Per-attachment outgoing media flow. Candidate discovery is deliberately outside this coordinator. */
internal class RcsMediaSendCoordinator(
    private val files: RcsMediaPrivateFiles,
    private val queue: RcsMediaTransferQueue,
    private val core: RcsMediaSendCore,
    private val go: RcsMediaSendGo,
    private val send: (RcsNativeCommand, UInt, () -> Unit) -> RcsNativeSendOutcome,
    private val nowMs: () -> Long,
    private val statusChanged: () -> Unit = {},
) {
    fun submit(attachment: RcsMediaAttachment): Boolean {
        return queue.submit(attachment.identity, statusChanged) { lease ->
            if (!lease.isCurrent()) return@submit
            if (!attachment.uploadRequired) {
                runNative(lease, attachment)
                return@submit
            }
            val plaintext = try { core.decrypt(attachment.attachmentId, files.rootForTest().absolutePath) } catch (_: Throwable) {
                publish(lease, attachment, "failed"); return@submit
            }
            try {
                val path = files.safePath(File(plaintext)) ?: run {
                    publish(lease, attachment, "failed")
                    return@submit
                }
                try {
                    core.markUploading(attachment)
                } catch (_: Throwable) {
                    publish(lease, attachment, "failed")
                    return@submit
                }
                if (!lease.isCurrent()) return@submit
                val upload = try {
                    go.upload(path, attachment)
                } catch (_: Throwable) {
                    publish(lease, attachment, "failed")
                    return@submit
                }
                if (upload.code != "ok" || upload.mediaRef == null) {
                    publish(lease, attachment, uploadOutcome(upload))
                    return@submit
                }
                core.markReferenceReady(attachment, upload.mediaRef, nowMs())
                if (!lease.isCurrent()) return@submit
                runNative(lease, attachment)
            } finally {
                files.delete(File(plaintext))
            }
        }
    }

    private fun runNative(lease: RcsMediaTransferQueue.Lease, attachment: RcsMediaAttachment) {
        val outcome = try { send(RcsNativeCommand(attachment.commandId, attachment.conversationId, attachment.recipient), attachment.attachmentIndex) { lease.afterRelease(statusChanged) } }
        catch (_: Throwable) { RcsNativeSendOutcome.OUTCOME_UNKNOWN }
        when (outcome) {
            RcsNativeSendOutcome.OUTCOME_UNKNOWN -> publish(lease, attachment, "outcome_unknown")
            RcsNativeSendOutcome.MEMBERS_CHANGED -> publish(lease, attachment, "members_changed")
            RcsNativeSendOutcome.REFUSED -> Unit
            else -> lease.afterRelease(statusChanged)
        }
    }

    private fun publish(lease: RcsMediaTransferQueue.Lease, attachment: RcsMediaAttachment, outcome: String) {
        if (lease.publish {
            core.recordOutcome(attachment, outcome, nowMs())
        }) lease.afterRelease(statusChanged)
    }

    private fun uploadOutcome(upload: RcsMediaUploadResult): String = when (upload.code) {
        "invalid_request", "not_found" -> "rejected"
        "failed", "busy", "unknown", "rejected", "not_allowed_mime", "too_large" -> upload.code
        else -> "failed"
    }
}

internal data class RcsMediaAttachment(val commandId: String, val conversationId: String, val recipient: String, val attachmentIndex: UInt, val attachmentId: String, val mime: String, val name: String, val size: ULong, val uploadRequired: Boolean = true) {
    val identity: String get() = "$commandId/$attachmentIndex"
}
internal data class RcsMediaUploadResult(val code: String, val mediaRef: String?)
internal interface RcsMediaSendCore {
    fun decrypt(attachmentId: String, destinationDir: String): String
    fun markUploading(attachment: RcsMediaAttachment)
    fun markReferenceReady(attachment: RcsMediaAttachment, mediaRef: String, uploadedAtMs: Long)
    fun recordOutcome(attachment: RcsMediaAttachment, outcome: String, atMs: Long)
}
internal interface RcsMediaSendGo {
    fun upload(path: File, attachment: RcsMediaAttachment): RcsMediaUploadResult
}

/** LinkHandle adapter for the upload-only half of a media send. Sending stays in RcsNativeSendCoordinator. */
internal class RcsMediaSendGoAdapter(private val lease: LinkHandleLease) : RcsMediaSendGo {
    override fun upload(path: File, attachment: RcsMediaAttachment): RcsMediaUploadResult {
        val request = JSONObject()
            .put("path", path.absolutePath)
            .put("mime", attachment.mime)
            .put("name", attachment.name)
            .put("size", attachment.size.toLong())
            .toString()
        val response = LinkRuntime.withCapturedLinkOperation(lease) { it.uploadRcsMedia(request) }
            ?: return RcsMediaUploadResult("rejected", null)
        val parsed = JSONObject(response)
        return RcsMediaUploadResult(parsed.optString("code", "failed"), parsed.optString("media_ref").takeIf(String::isNotEmpty))
    }
}
