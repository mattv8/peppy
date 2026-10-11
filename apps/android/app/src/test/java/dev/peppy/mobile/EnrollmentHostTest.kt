package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.generateHostedPassphrase

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class EnrollmentHostTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Test fun malformedDiscoveryAndInvalidPassphraseAreSanitized() {
        val store = MemorySecureStore()
        var installs = 0
        val host = EnrollmentHost(context, store, { installs += 1; ImportResult.IMPORTED })

        assertNull(host.claimedState("not-json"))
        assertEquals(
            ImportResult.SERVER_REJECTED,
            host.claimOwner("http://127.0.0.1:1", "setup", "password long enough", "weak"),
        )
        assertEquals(0, installs)
        assertTrue(store.values.isEmpty())
        host.close()
    }

    @Test fun ownerRetryRestoresOpaqueCheckpointInsteadOfGeneratingAnotherIdentity() {
        val store = MemorySecureStore()
        val requests = mutableListOf<JSONObject>()
        val transport = EnrollmentTransport { _, _, path, body, _ ->
            if (path == "/v1/instance/claim") {
                assertTrue(store.values.isNotEmpty())
                requests += JSONObject(body!!)
            }
            null
        }
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        EnrollmentHost(context, store, transport = transport).use { first ->
            assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "setup", "password long enough", secret))
            assertTrue(first.hasPendingOwner(origin))
        }
        EnrollmentHost(context, store, transport = transport).use { restored ->
            assertTrue(restored.hasPendingOwner(origin))
            assertEquals(ImportResult.NETWORK_ERROR, restored.claimOwner(origin, "setup", "password long enough", secret))
        }
        assertEquals(2, requests.size)
        assertEquals(requests[0].getString("operation_id"), requests[1].getString("operation_id"))
        assertEquals(
            requests[0].getJSONObject("device").getString("device_id"),
            requests[1].getJSONObject("device").getString("device_id"),
        )
        assertFalse(store.values.values.any { it.toString(Charsets.UTF_8).contains(secret) })
    }

    @Test fun scannerGateRejectsDuplicateFramesAndLateCallbacks() {
        val gate = EnrollmentScanGate()
        assertTrue(gate.tryStart())
        assertFalse(gate.tryStart())
        gate.cancel()
        assertFalse(gate.tryStart())
        assertFalse(gate.acceptResult())
    }

    @Test fun claimedPhoneStatusProducesSasBeforeApprovalAndRetainsSharedSecretMethod() {
        val origin = "https://enroll.example"
        val fixture = EnrollmentNativeFixture(origin)
        val transport = EnrollmentTransport { _, method, path, body, _ ->
            when {
                path.endsWith("/claim") -> fixture.claimResponse(body!!)
                method == "GET" -> fixture.claimedStatus()
                else -> null
            }
        }
        val flow = requireNotNull(EnrollmentHost(context, MemorySecureStore(), transport = transport).beginPhoneClaim(fixture.qr))
        try {
            val status = requireNotNull(flow.status())
            assertEquals(EnrollmentHost.PhoneState.CLAIMED, flow.state(status))
            assertNotNull(flow.sas(status))
            assertTrue(flow.requiresSharedSecret())
            assertNotEquals(EnrollmentHost.PhoneState.APPROVED, flow.state(status))
            assertEquals(EnrollmentHost.PhoneState.DENIED, flow.state("{\"state\":\"denied\"}"))
            assertEquals(EnrollmentHost.PhoneState.EXPIRED, flow.state("{\"state\":\"expired\"}"))
        } finally {
            flow.close()
            fixture.close()
        }
    }

    private class EnrollmentNativeFixture(private val origin: String) : AutoCloseable {
        private val invitationId = "00000000-0000-0000-0000-000000000007"
        private val invitationToken = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 2 })
        private val ownerTransferKey = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 3 })
        private lateinit var claimRequest: JSONObject
        val qr: String = JSONObject()
            .put("version", 1)
            .put("purpose", "connect_phone")
            .put("origin", origin)
            .put("invitation_token", invitationToken)
            .put("transfer_public_key", ownerTransferKey)
            .toString()

        fun claimResponse(body: String): String {
            claimRequest = JSONObject(body)
            return JSONObject()
                .put("invitation_id", invitationId)
                .put("claim_secret", java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 4 }))
                .put("origin", origin)
                .put("vault_id", "00000000-0000-0000-0000-000000000008")
                .put("fingerprint", "ab".repeat(32))
                .put("key_epoch", 1)
                .put("method", "shared_secret")
                .put("owner_transfer_public_key", ownerTransferKey)
                .put("expires_in_seconds", 300)
                .toString()
        }

        fun claimedStatus(): String {
            val device = claimRequest.getJSONObject("device")
            val transcript = JSONObject()
                .put("origin", origin)
                .put("vault_id", "00000000-0000-0000-0000-000000000008")
                .put("fingerprint", "ab".repeat(32))
                .put("key_epoch", 1)
                .put("enrollment_id", invitationId)
                .put("nonce", claimRequest.getString("invitation_token_digest"))
                .put("owner_transfer_public_key", ownerTransferKey)
                .put("recipient_transfer_public_key", claimRequest.getString("transfer_public_key"))
                .put("signing_public_key", device.getString("signing_public_key"))
                .put("device_id", device.getString("device_id"))
                .put("credential_digest", claimRequest.getString("credential_digest"))
                .put("role", "gateway")
            val claim = JSONObject()
                .put("device", device)
                .put("transfer_public_key", claimRequest.getString("transfer_public_key"))
                .put("credential_digest", claimRequest.getString("credential_digest"))
                .put("claim_authenticator", claimRequest.getString("claim_authenticator"))
            return JSONObject()
                .put("state", "claimed")
                .put("expires_in_seconds", 300)
                .put("claim", claim)
                .put("transcript", transcript)
                .toString()
        }

        override fun close() = Unit
    }

    private class MemorySecureStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        override fun read(key: String) = values[key]?.copyOf()
        override fun write(key: String, value: ByteArray) { values[key] = value.copyOf() }
        override fun remove(key: String) { values.remove(key) }
    }
}
