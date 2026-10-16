package dev.peppy.mobile

import org.json.JSONObject
import android.content.Context
import uniffi.peppy_mobile_bindings.NativeCipherObject
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPause
import java.io.File
import java.util.UUID

data class MmsTransferFailure(val attachmentId: String, val operation: String, val reason: String)
data class MmsTransferResult(val more: Boolean, val uploaded: Int, val downloaded: Int, val failures: List<MmsTransferFailure>)

/** Bounded cipher-file transfer. Core retains all encryption and verification authority. */
/**
 * [referenceTracked] marks uploads (contact photos) whose server reservation must opt into
 * reference tracking so photo-bearing envelopes can be registered and later reclaimed.
 */
class MmsMediaTransfer(
    private val client: NativeClientInterface,
    private val http: GatewayHttp,
    private val cacheDirectory: File,
    private val referenceTracked: (String) -> Boolean = { false },
    private val uploadsAllowed: Boolean = true,
    private val onVaultQuotaExceeded: () -> Unit = {},
    private val onUploadSucceeded: () -> Unit = {},
) {
    fun run(limit: Int = 4): MmsTransferResult {
        val budget = limit.coerceIn(1, MAX_BATCH); val failures = mutableListOf<MmsTransferFailure>(); var uploaded = 0; var downloaded = 0; var attempted = 0
        val uploads = client.pendingUploads(); val downloads = client.pendingDownloads()
        val upBudget = if (uploadsAllowed) (budget + 1) / 2 else 0
        var quotaExceeded = !uploadsAllowed
        for (objectToUpload in uploads.take(upBudget)) {
            attempted++
            if (upload(objectToUpload, failures)) uploaded++
            if (failures.lastOrNull()?.reason == "quota") {
                quotaExceeded = true
                break
            }
        }
        for (objectToDownload in downloads.take(budget - attempted)) {
            attempted++
            if (download(objectToDownload, failures)) downloaded++
        }
        if (!quotaExceeded && attempted < budget) for (objectToUpload in uploads.drop(upBudget).take(budget - attempted)) {
            attempted++
            if (upload(objectToUpload, failures)) uploaded++
            if (failures.lastOrNull()?.reason == "quota") break
        }
        val more = if (quotaExceeded) downloads.size > downloaded else uploads.size + downloads.size > attempted && failures.size < attempted
        return MmsTransferResult(more, uploaded, downloaded, failures)
    }

    private fun upload(item: NativeCipherObject, failures: MutableList<MmsTransferFailure>): Boolean {
        return try {
        val id = uuid(item.attachmentId); val file = File(client.nativeCipherFileForUpload(id))
        val bytes = item.ciphertextBytes.toLong()
        if (!file.isFile || bytes !in 1..GatewayHttp.MAX_MEDIA_BYTES || file.length() != bytes || !sha(item.ciphertextSha256)) return fail(failures, item.attachmentId, "upload", "invalid_media")
        val reserve = JSONObject().put("attachment_id", id).put("declared_ciphertext_bytes", bytes).put("declared_ciphertext_sha256", item.ciphertextSha256)
            .apply { if (referenceTracked(id)) put("reference_tracking", true) }.toString()
        val reservation = http.postJson("/v1/attachments/reserve", reserve)
        if (!reservation.ok && reservation.code != 409) {
            if (reservation.errorCode == "vault_quota_exceeded") onVaultQuotaExceeded()
            return fail(failures, id, "upload", reason(reservation))
        }
        if (reservation.ok) { val remote = uuid(JSONObject(reservation.body ?: throw IllegalArgumentException()).getString("attachment_id")); if (remote != id) return fail(failures, id, "upload", "permanent") }
        val put = if (reservation.ok) http.putFile("/v1/attachments/$id/upload", file, bytes) else null
        if (put != null && !put.ok && put.code !in setOf(404,409)) return fail(failures, id, "upload", reason(put))
        val finalized = http.postJson("/v1/attachments/$id/finalize", "")
        if (!finalized.ok) return fail(failures, id, "upload", reason(finalized))
        val proof = JSONObject(finalized.body ?: throw IllegalArgumentException())
        val finalizedId = uuid(proof.getString("attachment_id"))
        val validProof = finalizedId == id && (reservation.ok || proof.optBoolean("duplicate", false))
        if (!validProof) return fail(failures, id, "upload", "permanent")
        client.markAttachmentUploaded(id, id)
        onUploadSucceeded()
        true
    } catch (_: GatewayTransportException) { fail(failures, item.attachmentId, "upload", "transient") }
          catch (_: IllegalArgumentException) { fail(failures, item.attachmentId, "upload", "permanent") }
          catch (_: Exception) { fail(failures, item.attachmentId, "upload", "permanent") }
    }

    private fun download(item: NativeCipherObject, failures: MutableList<MmsTransferFailure>): Boolean {
        return try {
        val remote = uuid(item.remoteObjectId ?: return fail(failures, item.attachmentId, "download", "permanent"))
        uuid(item.attachmentId); val bytes = item.ciphertextBytes.toLong()
        if (bytes !in 1..GatewayHttp.MAX_MEDIA_BYTES || !sha(item.ciphertextSha256)) return fail(failures, item.attachmentId, "download", "invalid_media")
        cacheDirectory.mkdirs(); val temp = File.createTempFile("mms-cipher-", ".part", cacheDirectory)
        try {
            val response = http.getToFile("/v1/attachments/$remote", temp, bytes)
            if (!response.ok) return fail(failures, item.attachmentId, "download", reason(response))
            if (temp.length() != bytes) return fail(failures, item.attachmentId, "download", "invalid_media")
            client.installDownloadedAttachment(item.attachmentId, temp.path); true
        } finally { temp.delete() }
        } catch (_: GatewayTransportException) { fail(failures, item.attachmentId, "download", "transient") }
          catch (_: java.io.IOException) { fail(failures, item.attachmentId, "download", "transient") }
          catch (_: IllegalArgumentException) { fail(failures, item.attachmentId, "download", "permanent") }
          catch (_: Exception) { fail(failures, item.attachmentId, "download", "invalid_media") }
    }

    private fun fail(failures: MutableList<MmsTransferFailure>, id: String, operation: String, reason: String): Boolean { failures += MmsTransferFailure(id, operation, reason); return false }
    private fun uuid(value: String): String = UUID.fromString(value).toString()
    private fun sha(value: String) = value.matches(Regex("[0-9a-f]{64}"))
    private fun reason(result: HttpResult): String = when {
        result.code == 401 || result.code == 403 -> "auth"
        result.errorCode == "vault_quota_exceeded" -> "quota"
        result.code == 408 || result.code == 429 || result.code >= 500 || result.errorCode in setOf("attachment_upload_in_progress", "attachment_unavailable") -> "transient"
        result.code in 300..399 || result.code in 400..499 -> "permanent"
        else -> "transient"
    }
    companion object { const val MAX_BATCH = 50 }
}

/** Persists only the fact that uploads were refused for quota; capacity remains core-owned. */
internal class MmsRemoteQuota(private val context: Context, private val client: NativeClientInterface) {
    private val preferences = context.applicationContext.getSharedPreferences("peppy-mms-remote-quota", Context.MODE_PRIVATE)

    fun uploadsAllowed(http: GatewayHttp): Boolean {
        if (!pausedByCoreOrServer()) return true
        val response = try { http.get("/v1/attachments/usage") } catch (_: Exception) { return false }
        val usage = parsePositiveHeadroom(response) ?: return false
        if (!usage) return false
        preferences.edit().putBoolean(PAUSED, false).apply()
        client.reportMmsBackfillRemoteCapacity(generation(), MmsCapture.PROVIDER_SCOPE, true)
        return true
    }

    fun pause() {
        preferences.edit().putBoolean(PAUSED, true).apply()
        client.reportMmsBackfillVaultQuotaExceeded(generation(), MmsCapture.PROVIDER_SCOPE)
    }

    fun confirmUploadCapacity() {
        preferences.edit().putBoolean(PAUSED, false).apply()
        client.reportMmsBackfillRemoteCapacity(generation(), MmsCapture.PROVIDER_SCOPE, true)
    }

    private fun pausedByCoreOrServer(): Boolean = preferences.getBoolean(PAUSED, false) ||
        client.mmsBackfillStatus(generation(), MmsCapture.PROVIDER_SCOPE).pause == NativeMmsBackfillPause.REMOTE_STORAGE

    private fun generation() = MmsPreferences(context).sourceGeneration

    private fun parsePositiveHeadroom(response: HttpResult): Boolean? {
        if (!response.ok) return null
        return try {
            val usage = JSONObject(response.body ?: return null)
            val used = usage.optLong("used_bytes", -1)
            val limit = usage.optLong("limit_bytes", -1)
            if (used < 0 || limit <= 0) false else used < limit
        } catch (_: Exception) { null }
    }

    private companion object { const val PAUSED = "paused" }
}
