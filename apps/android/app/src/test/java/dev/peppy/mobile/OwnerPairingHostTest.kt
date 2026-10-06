package dev.peppy.mobile

import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.pairingIntentSas
import uniffi.peppy_mobile_bindings.vaultProfileFingerprint
import java.util.UUID
import java.util.Base64

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class OwnerPairingHostTest {
    private val profile = SharedVault.material.profileJson
    private val origin = TEST_ORIGIN
    private val vault = SharedVault.vaultId
    private val owner = SharedVault.gatewayDeviceId
    private val token = Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 7 })
    private val claimant = UUID.randomUUID().toString()
    private val digest = "b".repeat(64)

    @Test fun lockedOrNonOwnerNeverPostsCreate() {
        val transport = FakeTransport(role = "gateway")
        assertNull(OwnerPairingHost({ null }, transport).create())
        assertNull(host(transport).create())
        assertFalse(transport.posts.any { it.path == "/v1/pairing/intents" })
    }

    @Test fun rejectsForeignOriginAndMalformedIntent() {
        val transport = FakeTransport(createOrigin = "https://evil.example")
        assertNull(host(transport).create())
        transport.createOrigin = origin; transport.createToken = "short"
        assertNull(host(transport).create())
    }

    @Test fun changedIdentityNeverSendsOldIntentToNewOrigin() {
        var facts = facts()
        val transport = FakeTransport()
        val host = OwnerPairingHost({ facts }, transport)
        val intent = checkNotNull(host.create())
        facts = facts(origin = "https://other.example")
        assertNull(host.status(intent))
        assertFalse(transport.gets.any { it.path.contains(intent.token) })
    }

    @Test fun mismatchConfirmationOrChangedClaimNeverApproves() {
        val transport = FakeTransport(claimDigest = digest)
        val host = host(transport); val intent = checkNotNull(host.create()); val claim = checkNotNull(host.status(intent))
        assertFalse(host.approve(intent, claim, false))
        transport.claimDigest = "c".repeat(64)
        assertFalse(host.approve(intent, claim, true))
        assertFalse(transport.posts.any { it.path.endsWith("/approve") })
    }

    @Test fun requestedRoleIncludedInEquality() {
        val transport = FakeTransport(claimDigest = digest)
        val host = host(transport); val intent = checkNotNull(host.create()); val claim = checkNotNull(host.status(intent))
        assertTrue(claim.requestedRole == "gateway")
        transport.statusResponse = JSONObject().put("claimed", true).put("approved", false).put("device_id", claimant).put("key_digest", digest).put("requested_role", "device").put("sas", pairingIntentSas(token, digest, claimant)).put("expires_in_seconds", 100).toString()
        val claimDevice = checkNotNull(host.status(intent))
        assertTrue(claimDevice.requestedRole == "device")
        assertFalse(claimDevice == claim)
    }

    @Test fun profileMismatchNeverApprovesAndSuccessUsesOnlyRequiredFields() {
        val transport = FakeTransport(claimDigest = digest)
        val host = host(transport); val intent = checkNotNull(host.create()); val claim = checkNotNull(host.status(intent))
        transport.serverFingerprint = "d".repeat(64)
        assertFalse(host.approve(intent, claim, true))
        transport.serverFingerprint = null
        transport.epoch += 1
        assertFalse(host.approve(intent, claim, true))
        transport.epoch -= 1
        assertNotNull(host.status(intent))
        assertTrue(host.approve(intent, claim, true))
        val body = JSONObject(transport.posts.last { it.path.endsWith("/approve") }.body)
        assertTrue(body.length() == 3 && body.has("key_digest") && body.has("profile_fingerprint") && body.has("key_epoch"))
    }

    @Test fun non2xxResponseRejectedInCreate() {
        val transport = FakeTransport()
        transport.vaultCode = 502
        val host = host(transport)
        assertNull(host.create())
    }

    @Test fun non2xxResponseRejectedInStatus() {
        val transport = FakeTransport(claimDigest = digest)
        val host = host(transport)
        val intent = checkNotNull(host.create())
        transport.statusCode = 502
        assertNull(host.status(intent))
    }

    @Test fun malformedJsonRejectedInCreate() {
        val transport = FakeTransport()
        transport.createResponse = "not json"
        val host = host(transport)
        assertNull(host.create())
    }

    @Test fun malformedJsonRejectedInStatus() {
        val transport = FakeTransport(claimDigest = digest)
        val host = host(transport)
        val intent = checkNotNull(host.create())
        transport.statusResponse = ""
        assertNull(host.status(intent))
    }

    @Test fun computerOriginMismatchMakesNoHttpCalls() {
        val transport = FakeTransport()
        val host = computerHost(transport)
        val result = host.startComputerPairing("foreign")
        assertTrue(result is OwnerPairingComputerResult.Error)
        assertTrue(transport.gets.isEmpty() && transport.posts.isEmpty())
    }

    @Test fun computerOfferUsesCreatedIntentDigestAndSealedToken() {
        val transport = FakeTransport()
        val host = computerHost(transport)
        val result = host.startComputerPairing("valid")
        assertTrue(result is OwnerPairingComputerResult.Ready)
        val offer = transport.posts.last()
        assertTrue(offer.path == "/v1/pairing/join-requests/$joinRequestId/offer")
        val body = JSONObject(offer.body)
        assertTrue(body.getString("intent_digest") == "digest-for-$token")
        assertTrue(body.getString("sealed_intent_token") == "sealed-token")
        assertTrue(transport.posts.all { it.facts == facts() })
    }

    @Test fun computerOfferMapsExpiredAndAlreadyOffered() {
        val transport = FakeTransport()
        val host = computerHost(transport)
        transport.offerResult = HttpResult(410, "{\"code\":\"join_request_expired\"}")
        assertTrue((host.startComputerPairing("valid") as OwnerPairingComputerResult.Error).error == OwnerPairingComputerError.EXPIRED)
        transport.offerResult = HttpResult(409, "{\"code\":\"join_request_already_offered\"}")
        assertTrue((host.startComputerPairing("valid") as OwnerPairingComputerResult.Error).error == OwnerPairingComputerError.ALREADY_LINKED)
    }

    @Test fun deviceClaimUsesComputerRoleAndOnlyConfirmedApprovalPosts() {
        val transport = FakeTransport(claimDigest = digest)
        val host = computerHost(transport)
        val ready = host.startComputerPairing("valid") as OwnerPairingComputerResult.Ready
        transport.statusResponse = JSONObject().put("claimed", true).put("approved", false).put("device_id", claimant).put("key_digest", digest).put("requested_role", "device").put("sas", pairingIntentSas(token, digest, claimant)).put("expires_in_seconds", 100).toString()
        val claim = checkNotNull(host.status(ready.intent))
        assertTrue(claim.requestedRole == "device")
        assertFalse(host.approve(ready.intent, claim, false, expectedRole = "device"))
        assertFalse(transport.posts.any { it.path.endsWith("/approve") })
        assertTrue(host.approve(ready.intent, claim, true, expectedRole = "device"))
    }

    @Test fun computerRejectsGatewayRole() {
        val transport = FakeTransport(claimDigest = digest)
        val host = computerHost(transport)
        val ready = host.startComputerPairing("valid") as OwnerPairingComputerResult.Ready
        val defaultClaim = checkNotNull(host.status(ready.intent))
        assertTrue(defaultClaim.requestedRole == "gateway")
        assertFalse(host.approve(ready.intent, defaultClaim, true, expectedRole = "device"))
        assertFalse(transport.posts.any { it.path.endsWith("/approve") })
    }

    @Test fun canceledApprovalNeverPosts() {
        val transport = FakeTransport()
        val host = host(transport)
        val intent = checkNotNull(host.create())
        val claim = checkNotNull(host.status(intent))
        assertFalse(host.approve(intent, claim, true, canContinue = { false }))
        assertFalse(transport.posts.any { it.path.endsWith("/approve") })
    }

    private fun facts(origin: String = this.origin) = OwnerPairingFacts(origin, vault, owner, profile)
    private fun host(transport: FakeTransport) = OwnerPairingHost({ facts() }, transport)
    private val joinRequestId = UUID.randomUUID().toString()
    private fun computerHost(transport: FakeTransport) = OwnerPairingHost({ facts() }, transport, FakeJoinRequestCrypto(origin, joinRequestId))
    private class Call(val path: String, val body: String = "", val facts: OwnerPairingFacts)
    private class FakeJoinRequestCrypto(private val origin: String, private val id: String) : OwnerPairingJoinRequestCrypto {
        override fun parse(payload: String, allowLoopbackHttp: Boolean) = if (payload == "valid") OwnerPairingJoinRequest(origin, id, "join-key") else OwnerPairingJoinRequest("https://other.example", id, "join-key")
        override fun seal(joinKey: String, intentToken: String) = "sealed-token"
        override fun digest(intentToken: String) = "digest-for-$intentToken"
    }
    private inner class FakeTransport(var role: String = "owner", var createOrigin: String = origin, var createToken: String = token, var claimDigest: String = digest) : OwnerPairingTransport {
        val gets = mutableListOf<Call>(); val posts = mutableListOf<Call>()
        var epoch = JSONObject(profile).getLong("key_epoch")
        var serverFingerprint: String? = null
        var vaultCode: Int = 200
        var statusCode: Int = 200
        var createResponse: String? = null
        var statusResponse: String? = null
        var offerResult: HttpResult? = null
        private val fingerprint get() = vaultProfileFingerprint(profile)
        override fun get(facts: OwnerPairingFacts, path: String): HttpResult {
            gets += Call(path, facts = facts)
            return if (path == "/v1/vault") {
                if (vaultCode != 200) HttpResult(vaultCode, "") else HttpResult(200, JSONObject().put("role", role).put("vault_id", vault).put("device_id", owner).put("key_epoch", epoch).put("profile_fingerprint", serverFingerprint ?: fingerprint).toString())
            } else {
                if (statusCode != 200) HttpResult(statusCode, "") else HttpResult(200, statusResponse ?: JSONObject().put("claimed", true).put("approved", false).put("device_id", claimant).put("key_digest", claimDigest).put("requested_role", "gateway").put("sas", pairingIntentSas(token, claimDigest, claimant)).put("expires_in_seconds", 100).toString())
            }
        }
        override fun post(facts: OwnerPairingFacts, path: String, body: String): HttpResult {
            posts += Call(path, body, facts)
            return when (path) {
                "/v1/pairing/intents" -> HttpResult(200, createResponse ?: JSONObject().put("https_origin", createOrigin).put("intent_token", createToken).put("expires_in_seconds", 120).toString())
                "/v1/pairing/join-requests/$joinRequestId/offer" -> offerResult ?: HttpResult(200, "{}")
                else -> HttpResult(200, "{}")
            }
        }
    }
}
