package dev.peppy.mobile

import uniffi.peppy_mobile_bindings.NativeEnrollmentClient
import uniffi.peppy_mobile_bindings.NativeEnrollmentMethod
import uniffi.peppy_mobile_bindings.NativeOwnerPhoneStage
import uniffi.peppy_mobile_bindings.ownerPhoneShouldRotate

/** Thin adapter; Rust remains the source of owner invitation validation and policy. */
internal class NativeOwnerPhoneClient(private val native: NativeEnrollmentClient) : OwnerPhoneClient {
    override fun createInvitationRequest(method: OwnerPhoneMethod): String =
        native.createPhoneInvitationRequest(method.native())

    override fun consumeCreated(createdJson: String) = native.consumeCreatedPhoneInvitation(createdJson)
    override fun qrPayload(): String = native.ownerPhoneQrPayload()
    override fun invitationId(): String? = native.ownerPhoneInvitationId()
    override fun method(): OwnerPhoneMethod? = native.ownerPhoneMethod()?.owner()
    override fun secretMatches(secret: CharArray): Boolean = native.ownerPassphraseMatches(secret.concatToString())
    override fun approveRequest(statusJson: String, secret: CharArray): String =
        native.approvePhoneInvitationRequest(statusJson, secret.concatToString())
    override fun close() = native.close()

    override fun status(statusJson: String): OwnerPhoneStatus {
        val value = native.ownerPhoneStatus(statusJson)
        return OwnerPhoneStatus(
            stage = value.stage.owner(),
            expiresInSeconds = value.expiresInSeconds.toInt(),
            sas = value.sas,
            claimDigest = value.claimDigest,
            invitationId = value.invitationId,
        )
    }
}

internal fun nativeOwnerPhoneShouldRotate(
    stage: OwnerPhoneStage,
    expiresInSeconds: Int,
    active: OwnerPhoneMethod,
    requested: OwnerPhoneMethod,
): Boolean = ownerPhoneShouldRotate(stage.native(), expiresInSeconds.toUInt(), active.native(), requested.native())

private fun OwnerPhoneMethod.native(): NativeEnrollmentMethod = when (this) {
    OwnerPhoneMethod.TRANSFER -> NativeEnrollmentMethod.TRANSFER
    OwnerPhoneMethod.SHARED_SECRET -> NativeEnrollmentMethod.SHARED_SECRET
}

private fun NativeEnrollmentMethod.owner(): OwnerPhoneMethod = when (this) {
    NativeEnrollmentMethod.TRANSFER -> OwnerPhoneMethod.TRANSFER
    NativeEnrollmentMethod.SHARED_SECRET -> OwnerPhoneMethod.SHARED_SECRET
}

private fun OwnerPhoneStage.native(): NativeOwnerPhoneStage = when (this) {
    OwnerPhoneStage.WAITING -> NativeOwnerPhoneStage.WAITING
    OwnerPhoneStage.VERIFY -> NativeOwnerPhoneStage.VERIFY
    OwnerPhoneStage.APPROVED -> NativeOwnerPhoneStage.APPROVED
    OwnerPhoneStage.CONNECTED -> NativeOwnerPhoneStage.CONNECTED
    OwnerPhoneStage.EXPIRED -> NativeOwnerPhoneStage.EXPIRED
    OwnerPhoneStage.DENIED -> NativeOwnerPhoneStage.DENIED
}

private fun NativeOwnerPhoneStage.owner(): OwnerPhoneStage = when (this) {
    NativeOwnerPhoneStage.WAITING -> OwnerPhoneStage.WAITING
    NativeOwnerPhoneStage.VERIFY -> OwnerPhoneStage.VERIFY
    NativeOwnerPhoneStage.APPROVED -> OwnerPhoneStage.APPROVED
    NativeOwnerPhoneStage.CONNECTED -> OwnerPhoneStage.CONNECTED
    NativeOwnerPhoneStage.EXPIRED -> OwnerPhoneStage.EXPIRED
    NativeOwnerPhoneStage.DENIED -> OwnerPhoneStage.DENIED
}
