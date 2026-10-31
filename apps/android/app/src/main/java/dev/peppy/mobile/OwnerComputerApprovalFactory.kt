package dev.peppy.mobile

import android.content.Context
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.newNativeExistingOwnerEnrollmentClient

/** Creates computer approval only from an unlocked, non-revoked protected owner identity. */
internal fun createOwnerComputerApprovalHost(context: Context): OwnerComputerApprovalHost? {
    val account = GatewayAccountHost.load(context) ?: return null
    if (account.self.role != "owner" || account.self.revoked) return null
    val material = NativeGateway.ownerEnrollmentMaterial(context) ?: return null
    val accountIdentity = NativeGateway.accountIdentity(context) ?: return null
    if (accountIdentity.origin != material.origin || accountIdentity.vaultId != material.vaultId || accountIdentity.deviceId != material.deviceId) return null
    val client = try {
        NativeOwnerComputerClient(newNativeExistingOwnerEnrollmentClient(material.origin, NativeEnrollmentClientKind.ANDROID, "Android phone", material.profileJson, material.headerJson)).also {
            it.configureOwnerDevice(material.deviceId)
        }
    } catch (_: Exception) { return null }
    val accountSession = ownerPhoneAccountSession(material.origin) {
        HostedAccountClient(AndroidHostedSecureStore(context)).enrollmentSessionHeader()
    }
    return OwnerComputerApprovalHost(
        client, OwnerPhoneTransport { method, path, body, session ->
            val credential = NativeGateway.currentAccountCredential(context, accountIdentity) ?: return@OwnerPhoneTransport null
            ownerComputerRequest(credential, method, path, body, session, material.retentionProtocolHeader)
        },
        VaultSecretStore(AndroidHostedSecureStore(context)), material.vaultId, material.deviceId,
        { NativeGateway.open(context) }, material.origin, material.origin == HostedAccountClient.ORIGIN, accountSession,
    )
}

private fun ownerComputerRequest(
    credential: GatewayAccountCredential,
    method: String,
    path: String,
    body: String?,
    accountSession: String?,
    retentionHeader: uniffi.peppy_mobile_bindings.NativeAttachmentRetentionProtocolHeader?,
) = ownerPhoneRequest(credential, method, path, body, accountSession, retentionHeader)
