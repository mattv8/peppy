package dev.peppy.mobile.enrollment

import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeEnrollmentClient
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.NativeEnrollmentIdentity
import uniffi.peppy_mobile_bindings.NativeEnrollmentMethod
import uniffi.peppy_mobile_bindings.newNativeEnrollmentClient
import uniffi.peppy_mobile_bindings.newNativeExistingOwnerEnrollmentClient

/** Builds only transport responses; every enrollment credential remains native-produced. */
internal class NativeEnrollmentFixture(
    private val origin: String,
    val passphrase: String,
) : AutoCloseable {
    private val client: NativeEnrollmentClient = newNativeEnrollmentClient(
        origin,
        NativeEnrollmentClientKind.ANDROID,
        "Robolectric enrollment fixture",
    )
    private val donorMaterial = ownerRequest().getJSONObject("material")
    private var manualDeviceId: String? = null

    fun ownerIdentity(): NativeEnrollmentIdentity {
        val request = ownerRequest()
        return client.finishOwnerAdmission(claimResponse(request).toString(), passphrase)
    }

    fun ownerRequest(): JSONObject = JSONObject(client.createOwnerRequest("setup", "admin", "password long enough", passphrase))

    fun manualChallenge(requestJson: String): JSONObject {
        val request = JSONObject(requestJson)
        val material = donorMaterial
        val profile = material.getJSONObject("profile")
        val device = request.getJSONObject("device")
        manualDeviceId = device.getString("device_id")
        return JSONObject()
            .put("enrollment_id", "00000000-0000-0000-0000-000000000007")
            .put("origin", origin)
            .put("vault_id", profile.getString("vault_id"))
            .put("profile", profile)
            .put("fingerprint", material.getString("fingerprint"))
            .put("key_epoch", profile.getInt("key_epoch"))
            .put("device", device)
            .put("requested_role", "gateway")
            .put("nonce", java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 7 }))
            .put("credential_digest", request.getString("credential_digest"))
            .put("expires_in_seconds", 300)
    }

    fun admissionComplete(requestJson: String): JSONObject {
        JSONObject(requestJson)
        val deviceId = checkNotNull(manualDeviceId) { "manual challenge must precede completion" }
        val material = donorMaterial
        return JSONObject()
            .put("vault_id", material.getJSONObject("profile").getString("vault_id"))
            .put("device_id", deviceId)
            .put("role", "gateway")
            .put("material", material)
    }

    fun phoneInvitation(method: NativeEnrollmentMethod): PhoneInvitationFixture {
        val material = donorMaterial
        val owner = newNativeExistingOwnerEnrollmentClient(
            origin,
            NativeEnrollmentClientKind.ANDROID,
            "fixture phone owner",
            material.getJSONObject("profile").toString(),
            String(java.util.Base64.getUrlDecoder().decode(material.getString("check_header")), Charsets.UTF_8),
        )
        val invitation = JSONObject(owner.createPhoneInvitationRequest(method))
        val invitationId = "00000000-0000-0000-0000-000000000009"
        owner.consumeCreatedPhoneInvitation(JSONObject().put("invitation_id", invitationId).put("expires_in_seconds", 300).toString())
        return PhoneInvitationFixture(owner, origin, material, invitation, invitationId, method, passphrase, owner.ownerPhoneQrPayload())
    }

    fun claimResponse(request: JSONObject): JSONObject {
        val material = request.getJSONObject("material")
        return JSONObject()
            .put("vault_id", material.getJSONObject("profile").getString("vault_id"))
            .put("device_id", request.getJSONObject("device").getString("device_id"))
            .put("replayed", false)
            .put(
                "session",
                JSONObject()
                    .put("session_token", "fixture-native-session")
                    .put("csrf_token", JSONObject.NULL)
                    .put("expires_in_seconds", 3600),
            )
    }

    override fun close() = client.close()

    internal class PhoneInvitationFixture(
        private val owner: NativeEnrollmentClient,
        private val origin: String,
        private val material: JSONObject,
        private val invitation: JSONObject,
        private val invitationId: String,
        private val method: NativeEnrollmentMethod,
        private val passphrase: String,
        val qrPayload: String,
    ) : AutoCloseable {
        private var claim: JSONObject? = null
        private var approved: JSONObject? = null

        fun consumeClaim(requestJson: String): String {
            val request = JSONObject(requestJson).also { claim = it }
            return JSONObject()
                .put("invitation_id", invitationId)
                .put("claim_secret", java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 4 }))
                .put("origin", origin)
                .put("vault_id", material.getJSONObject("profile").getString("vault_id"))
                .put("fingerprint", material.getString("fingerprint"))
                .put("key_epoch", 1)
                .put("method", if (method == NativeEnrollmentMethod.TRANSFER) "transfer" else "shared_secret")
                .put("owner_transfer_public_key", invitation.getString("transfer_public_key"))
                .put("expires_in_seconds", 300)
                .toString()
        }

        fun approve() {
            val approval = JSONObject(owner.approvePhoneInvitationRequest(status("claimed"), passphrase))
            approved = JSONObject(status("claimed"))
                .put("state", "approved")
                .put("encrypted_key_package", approval.get("encrypted_key_package"))
                .put("transfer_nonce", approval.get("transfer_nonce"))
                .put("profile", material.getJSONObject("profile"))
        }

        fun status(): String = approved?.toString() ?: status("claimed")

        fun admissionComplete(): String {
            val device = checkNotNull(claim).getJSONObject("device")
            return JSONObject()
                .put("vault_id", material.getJSONObject("profile").getString("vault_id"))
                .put("device_id", device.getString("device_id"))
                .put("role", "gateway")
                .put("material", material)
                .toString()
        }

        private fun status(state: String): String {
            val request = checkNotNull(claim)
            val device = request.getJSONObject("device")
            val transcript = JSONObject()
                .put("origin", origin)
                .put("vault_id", material.getJSONObject("profile").getString("vault_id"))
                .put("fingerprint", material.getString("fingerprint"))
                .put("key_epoch", 1)
                .put("enrollment_id", invitationId)
                .put("nonce", request.getString("invitation_token_digest"))
                .put("owner_transfer_public_key", invitation.getString("transfer_public_key"))
                .put("recipient_transfer_public_key", request.getString("transfer_public_key"))
                .put("signing_public_key", device.getString("signing_public_key"))
                .put("device_id", device.getString("device_id"))
                .put("credential_digest", request.getString("credential_digest"))
                .put("role", "gateway")
            return JSONObject()
                .put("state", state)
                .put("expires_in_seconds", 300)
                .put("claim", JSONObject().put("device", device).put("transfer_public_key", request.getString("transfer_public_key")).put("credential_digest", request.getString("credential_digest")).put("claim_authenticator", request.getString("claim_authenticator")))
                .put("transcript", transcript)
                .put("encrypted_key_package", JSONObject.NULL)
                .put("transfer_nonce", JSONObject.NULL)
                .put("profile", JSONObject.NULL)
                .toString()
        }

        override fun close() = owner.close()
    }
}
