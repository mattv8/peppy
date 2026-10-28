package dev.peppy.mobile

import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeEnrollmentClient

/** Thin adapter for the Rust-owned computer enrollment state machine. */
internal class NativeOwnerComputerClient(private val native: NativeEnrollmentClient) : OwnerComputerClient {
    override fun createRequest(qrJson: String) = native.ownerComputerCreateRequest(qrJson).let {
        OwnerComputerRequest(it.origin, it.invitationId, it.promptDigest, it.requestJson)
    }
    override fun consumeCreated(invitationId: String, promptDigest: String, createdJson: String) =
        native.consumeOwnerComputerCreated(invitationId, promptDigest, createdJson)
    override fun view() = native.ownerComputerView().let {
        OwnerComputerView(it.origin, it.invitationId, it.promptDigest, it.created, it.sas)
    }
    override fun approve(invitationId: String, promptDigest: String, vaultKeyPhrase: CharArray, nativeClient: NativeClient) =
        native.approveOwnerComputerRequest(invitationId, promptDigest, true, vaultKeyPhrase.concatToString(), nativeClient)
    override fun cancel() = native.cancelOwnerComputerFlow()
    fun configureOwnerDevice(deviceId: String) = native.configureOwnerComputerDevice(deviceId)
    override fun close() = native.close()
}
