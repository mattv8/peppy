package dev.peppy.mobile

import android.content.Context
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.newNativeExistingOwnerEnrollmentClient
import java.net.HttpURLConnection
import java.net.URL

/** Restores the owner-only Rust client from protected enrollment material. */
internal fun createOwnerPhoneApprovalHost(context: Context): OwnerPhoneApprovalHost? {
    val account = GatewayAccountHost.load(context) ?: return null
    if (account.self.role != "owner" || account.self.revoked) return null
    val material = NativeGateway.ownerEnrollmentMaterial(context) ?: return null
    val accountIdentity = NativeGateway.accountIdentity(context) ?: return null
    if (accountIdentity.origin != material.origin || accountIdentity.vaultId != material.vaultId || accountIdentity.deviceId != material.deviceId) return null
    val client = NativeOwnerPhoneClient(
        newNativeExistingOwnerEnrollmentClient(
            material.origin,
            NativeEnrollmentClientKind.ANDROID,
            "Android phone",
            material.profileJson,
            material.headerJson,
        ),
    )
    val accountSession = ownerPhoneAccountSession(material.origin) {
        HostedAccountClient(AndroidHostedSecureStore(context)).enrollmentSessionHeader()
    }
    return OwnerPhoneApprovalHost(
        client = client,
        transport = OwnerPhoneTransport { method, path, body, session ->
            val credential = NativeGateway.currentAccountCredential(context, accountIdentity) ?: return@OwnerPhoneTransport null
            ownerPhoneRequest(credential, method, path, body, session, material.retentionProtocolHeader)
        },
        secrets = VaultSecretStore(AndroidHostedSecureStore(context)),
        vaultId = material.vaultId,
        deviceId = material.deviceId,
        shouldRotate = ::nativeOwnerPhoneShouldRotate,
        hosted = material.origin == HostedAccountClient.ORIGIN,
        accountSession = accountSession,
    )
}

/** The Peppy Hosted account session is only ever sent to the hosted origin, never to a self-hosted server. */
internal fun ownerPhoneAccountSession(origin: String, sessionHeader: () -> String?): String? =
    if (origin == HostedAccountClient.ORIGIN) sessionHeader() else null

internal fun ownerPhoneRequest(
    credential: GatewayAccountCredential,
    method: String,
    path: String,
    body: String?,
    accountSession: String?,
    retentionHeader: uniffi.peppy_mobile_bindings.NativeAttachmentRetentionProtocolHeader? = null,
): EnrollmentHttpResponse? {
    if (!path.startsWith("/v1/enrollment/") || path.contains('#')) return null
    val connection = try { URL(credential.origin + path).openConnection() as HttpURLConnection } catch (_: Exception) { return null }
    return try {
        connection.instanceFollowRedirects = false
        connection.requestMethod = method
        connection.connectTimeout = 20_000
        connection.readTimeout = 20_000
        connection.setRequestProperty("Accept", "application/json")
        connection.setRequestProperty("Authorization", "Bearer ${credential.bearerToken}")
        accountSession?.let { connection.setRequestProperty("Peppy-Account-Session", it) }
        if (method != "GET") retentionHeader?.let { connection.setRequestProperty(it.name, it.value) }
        body?.let {
            val bytes = it.toByteArray(Charsets.UTF_8)
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.setFixedLengthStreamingMode(bytes.size)
            connection.outputStream.use { output -> output.write(bytes) }
        }
        val status = connection.responseCode
        val stream = if (status in 200..299) connection.inputStream else connection.errorStream
        EnrollmentHttpResponse(status, stream?.use { GatewayHttp.readBounded(it, 64 * 1024) })
    } catch (_: Exception) {
        null
    } finally {
        connection.disconnect()
    }
}
