package dev.peppy.mobile

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID
import uniffi.peppy_mobile_bindings.NativeDeviceRetentionAvailability
import uniffi.peppy_mobile_bindings.NativeStorageCleanupState
import uniffi.peppy_mobile_bindings.NativeStorageCleanupUnavailableReason
import uniffi.peppy_mobile_bindings.NativeRetentionNoticeState
import uniffi.peppy_mobile_bindings.attachmentRetentionNoticeAcknowledgementRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionNoticeCopySupported
import uniffi.peppy_mobile_bindings.attachmentRetentionResponseReadLimitBytes
import uniffi.peppy_mobile_bindings.attachmentRetentionStatusRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionSupportedNoticePolicyVersion
import uniffi.peppy_mobile_bindings.classifyDeviceRetentionAvailability
import uniffi.peppy_mobile_bindings.parseAttachmentRetentionStatus

internal data class GatewayAccountDevice(val id: String, val role: String, val revoked: Boolean, val keyEpoch: Long)
internal data class GatewayAccountState(val self: GatewayAccountDevice, val devices: List<GatewayAccountDevice>)

/** Authenticated account boundary. Credentials stay inside NativeGateway / GatewayHttp. */
internal object GatewayAccountHost {
    internal data class AttachmentStorageScope(val value: String, val identity: GatewayAccountIdentity)

    /** Called only by an IO coroutine; it captures the account identity before credentials are used. */
    fun attachmentStorageScope(context: Context): AttachmentStorageScope? {
        val identity = NativeGateway.accountIdentity(context) ?: return null
        if (!NativeGateway.isCurrentAccountIdentity(context, identity)) return null
        val value = "${identity.origin}\n${identity.vaultId}\n${identity.deviceId}\n${identity.generation}"
        return AttachmentStorageScope(UUID.nameUUIDFromBytes(value.toByteArray(Charsets.UTF_8)).toString(), identity)
    }

    fun loadAttachmentStorage(context: Context, scope: AttachmentStorageScope): AttachmentStorageLoad? {
        val request = attachmentRetentionStatusRequest()
        val credential = NativeGateway.currentAccountCredential(context, scope.identity) ?: return null
        val result = try {
            GatewayHttp(credential.origin, credential.bearerToken).retentionRequest(
                request, attachmentRetentionResponseReadLimitBytes(),
            )
        } catch (_: Exception) {
            return if (NativeGateway.isCurrentAccountIdentity(context, scope.identity)) AttachmentStorageLoad.TemporarilyUnavailable else null
        }
        if (!NativeGateway.isCurrentAccountIdentity(context, scope.identity)) return null
        if (result.code == request.expectedSuccessStatus.toInt()) {
            val status = try { parseAttachmentRetentionStatus(result.body ?: return AttachmentStorageLoad.TemporarilyUnavailable) } catch (_: Exception) { return AttachmentStorageLoad.TemporarilyUnavailable }
            return AttachmentStorageLoad.Available(
                AttachmentStorageSnapshot(status.usedBytes, status.limitBytes, status.baseBytes, status.cleanupState.toUiCleanup(), status.cleanupUnavailableReason?.toUiUnavailableReason(),
                    AttachmentStorageNotice(status.notice.policyVersion, status.notice.state.toUiNoticeState(), status.notice.activationAtSeconds),
                    attachmentRetentionNoticeCopySupported(status.notice.policyVersion)),
            )
        }
        if (result.code == 401) return AttachmentStorageLoad.AuthorizationFailure
        return when (classifyDeviceRetentionAvailability(result.code.toUShort(), result.body ?: "")) {
            NativeDeviceRetentionAvailability.UNSUPPORTED -> AttachmentStorageLoad.Unsupported
            NativeDeviceRetentionAvailability.TEMPORARILY_UNAVAILABLE -> AttachmentStorageLoad.TemporarilyUnavailable
            NativeDeviceRetentionAvailability.OTHER -> AttachmentStorageLoad.OrdinaryFailure
        }
    }

    /** Builds only the frozen supported-v1 request and rechecks the captured account around HTTP. */
    fun acknowledgeAttachmentStorageNotice(context: Context, scope: AttachmentStorageScope): Boolean {
        val policyVersion = attachmentRetentionSupportedNoticePolicyVersion()
        if (!attachmentRetentionNoticeCopySupported(policyVersion)) return false
        val request = try { attachmentRetentionNoticeAcknowledgementRequest(policyVersion) } catch (_: Exception) { return false }
        val credential = NativeGateway.currentAccountCredential(context, scope.identity) ?: return false
        val result = try { GatewayHttp(credential.origin, credential.bearerToken).retentionRequest(request, attachmentRetentionResponseReadLimitBytes()) } catch (_: Exception) { return false }
        return NativeGateway.isCurrentAccountIdentity(context, scope.identity) && result.code == request.expectedSuccessStatus.toInt()
    }

    fun pushConfig(context: Context): HttpResult {
        val http = currentAccountHttp(context) ?: throw java.io.IOException("not enrolled")
        return http.get("/v1/devices/self/push-config")
    }
    /** Uses the credential captured with the cleanup record; delayed cleanup cannot borrow new auth. */
    fun deleteWakeRoute(origin: String, bearerToken: String, routeId: String): HttpResult {
        if (!UUID_RE.matches(routeId)) throw java.io.IOException("invalid route")
        return GatewayHttp(origin, bearerToken).deleteJson("/v1/devices/self/wake-route?route_id=$routeId", "{}")
    }
    fun load(context: Context): GatewayAccountState? {
        val credential = NativeGateway.accountCredential(context) ?: return null
        val result = try { GatewayHttp(credential.origin, credential.bearerToken).get("/v1/devices") } catch (_: Exception) { return null }
        if (!result.ok) return null
        return try {
            val list = JSONObject(result.body ?: return null).getJSONArray("devices").devices()
            val self = list.firstOrNull { it.id == credential.deviceId } ?: return null
            GatewayAccountState(self, list)
        } catch (_: Exception) { null }
    }

    fun revoke(context: Context, target: String): Boolean {
        val identity = NativeGateway.accountIdentity(context) ?: return false
        val credential = NativeGateway.currentAccountCredential(context, identity) ?: return false
        if (!UUID_RE.matches(target)) return false
        val http = currentAccountHttp(context, identity) ?: return false
        val result = try { http.postJson("/v1/devices/$target/revoke", "{}") } catch (_: Exception) { return false }
        // A credential can be replaced while the request is in flight.  Never archive the newer
        // enrollment merely because an earlier identity successfully revoked itself.
        val stillCurrent = NativeGateway.accountCredential(context)
        return if (target == credential.deviceId) {
            val expectedIdentity = stillCurrent?.let { it.deviceId == credential.deviceId && it.bearerToken == credential.bearerToken } == true
            // A self-revoke can commit before a connection drops.  A now-invalid token (401) or
            // already-revoked conflict (409) is evidence sufficient to stop this exact identity;
            // do not apply that inference to another device's revoke request.
            expectedIdentity && (result.ok || result.code == 401 || result.code == 409) && NativeGateway.archiveEnrollment(context)
        } else result.ok
    }

    fun deleteVault(context: Context): Boolean {
        val identity = NativeGateway.accountIdentity(context) ?: return false
        val credential = NativeGateway.currentAccountCredential(context, identity) ?: return false
        val http = currentAccountHttp(context, identity) ?: return false
        val result = try { http.deleteJson("/v1/vault", JSONObject().put("vault_id", credential.vaultId).toString()) } catch (_: Exception) { return false }
        val stillCurrent = NativeGateway.accountCredential(context)
        return result.ok && stillCurrent?.let { it.deviceId == credential.deviceId && it.bearerToken == credential.bearerToken } == true &&
            NativeGateway.archiveEnrollment(context)
    }

    fun publishWake(context: Context, routeId: String, wakeCredential: String) {
        val identity = NativeGateway.accountIdentity(context) ?: throw java.io.IOException("not enrolled")
        val credential = NativeGateway.currentAccountCredential(context, identity) ?: throw java.io.IOException("not enrolled")
        publishWake(credential.origin, credential.bearerToken, routeId, wakeCredential)
    }
    /** Publishes only with the credential captured by the matching relay operation. */
    fun publishWake(origin: String, bearerToken: String, routeId: String, wakeCredential: String) {
        val connection = URL(origin + "/v1/devices/self/wake-route").openConnection() as HttpURLConnection
        try {
            val body = JSONObject().put("route_id", routeId).put("wake_credential", wakeCredential).toString().toByteArray()
            connection.instanceFollowRedirects = false; connection.requestMethod = "PUT"; connection.connectTimeout = GatewayHttp.TIMEOUT_MS; connection.readTimeout = GatewayHttp.TIMEOUT_MS
            connection.setRequestProperty("Authorization", "Bearer $bearerToken"); connection.setRequestProperty("Content-Type", "application/json"); connection.doOutput = true
            connection.outputStream.use { it.write(body) }
            if (connection.responseCode !in 200..299) throw java.io.IOException("route publisher rejected")
        } finally { connection.disconnect() }
    }

    private fun JSONArray.devices() = buildList {
        for (index in 0 until length()) {
            val item = getJSONObject(index); val id = item.getString("device_id")
            val role = item.getString("role").takeIf { it == "owner" || it == "gateway" || it == "device" } ?: continue
            if (!UUID_RE.matches(id)) continue
            add(GatewayAccountDevice(id, role, item.optBoolean("revoked"), item.optLong("key_epoch", 0)))
        }
    }
    private val UUID_RE = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", RegexOption.IGNORE_CASE)

    private fun currentAccountHttp(context: Context, identity: GatewayAccountIdentity? = NativeGateway.accountIdentity(context)): GatewayHttp? {
        val captured = identity ?: return null
        return NativeGateway.currentAccountHttp(context, captured)
            ?: NativeGateway.currentAccountCredential(context, captured)?.let { GatewayHttp(it.origin, it.bearerToken) }
    }
}

private fun NativeStorageCleanupState.toUiCleanup() = when (this) {
    NativeStorageCleanupState.DISABLED -> AttachmentStorageCleanup.DISABLED
    NativeStorageCleanupState.BLOCKED -> AttachmentStorageCleanup.BLOCKED
    NativeStorageCleanupState.PENDING -> AttachmentStorageCleanup.PENDING
    NativeStorageCleanupState.AVAILABLE -> AttachmentStorageCleanup.AVAILABLE
}

private fun NativeRetentionNoticeState.toUiNoticeState() = when (this) {
    NativeRetentionNoticeState.NOT_PRESENTED -> AttachmentStorageNoticeState.NOT_PRESENTED
    NativeRetentionNoticeState.GRACE_PERIOD -> AttachmentStorageNoticeState.GRACE_PERIOD
    NativeRetentionNoticeState.ELIGIBLE -> AttachmentStorageNoticeState.ELIGIBLE
}

private fun NativeStorageCleanupUnavailableReason.toUiUnavailableReason() = when (this) {
    NativeStorageCleanupUnavailableReason.NOTICE_NOT_ELIGIBLE -> AttachmentStorageUnavailableReason.NOTICE_NOT_ELIGIBLE
    NativeStorageCleanupUnavailableReason.DEVICE_NOT_READY -> AttachmentStorageUnavailableReason.DEVICE_NOT_READY
    NativeStorageCleanupUnavailableReason.READ_ONLY -> AttachmentStorageUnavailableReason.READ_ONLY
    NativeStorageCleanupUnavailableReason.ENTITLEMENT_UNAVAILABLE -> AttachmentStorageUnavailableReason.ENTITLEMENT_UNAVAILABLE
    NativeStorageCleanupUnavailableReason.PROVIDER_RECONCILIATION_PENDING -> AttachmentStorageUnavailableReason.PROVIDER_RECONCILIATION_PENDING
    NativeStorageCleanupUnavailableReason.BILLING_MUTATION_PENDING -> AttachmentStorageUnavailableReason.BILLING_MUTATION_PENDING
    NativeStorageCleanupUnavailableReason.CLEANUP_DISABLED -> AttachmentStorageUnavailableReason.CLEANUP_DISABLED
}
