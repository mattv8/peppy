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
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.newNativeEnrollmentClient
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

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
            host.claimOwner("http://127.0.0.1:1", "admin", "setup", "password long enough", "weak"),
        )
        assertEquals(0, installs)
        assertTrue(store.values.isEmpty())
        host.close()
    }

    @Test fun discoveryRequiresActualJsonBooleansAndDefaultsMissingPolicyToProtected() {
        assertEquals(InstanceDiscovery(false, false), parseInstanceDiscovery("{\"claimed\":false,\"setup_required\":false}"))
        assertEquals(InstanceDiscovery(false, true), parseInstanceDiscovery("{\"claimed\":false}"))
        assertNull(parseInstanceDiscovery("{\"claimed\":\"false\",\"setup_required\":false}"))
        assertEquals(InstanceDiscovery(false, true), parseInstanceDiscovery("{\"claimed\":false,\"setup_required\":\"false\"}"))
    }

    @Test fun claimErrorsUseOnlyWhitelistedJsonCodes() {
        assertEquals(ImportResult.SETUP_TOKEN_REJECTED, claimErrorResult("{\"code\":\"invalid_setup_token\"}"))
        assertEquals(ImportResult.INSTANCE_ALREADY_CLAIMED, claimErrorResult("{\"code\":\"instance_already_claimed\"}"))
        assertNull(claimErrorResult("invalid_setup_token"))
        assertNull(claimErrorResult("{\"code\":\"unexpected\"}"))
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
        EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "admin", "setup", "password long enough", secret))
            assertTrue(first.hasPendingOwner(origin))
        }
        EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertTrue(restored.hasPendingOwner(origin))
            assertEquals(ImportResult.NETWORK_ERROR, restored.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        assertEquals(2, requests.size)
        assertEquals(requests[0].getString("operation_id"), requests[1].getString("operation_id"))
        assertEquals(
            requests[0].getJSONObject("device").getString("device_id"),
            requests[1].getJSONObject("device").getString("device_id"),
        )
        // The pending slot intentionally holds the recoverable secret in secure storage; checkpoints must stay opaque.
        assertFalse(store.values.filterKeys { !it.startsWith("vault-secret:pending:") }.values.any { it.toString(Charsets.UTF_8).contains(secret) })
    }

    @Test fun openOwnerClaimOmitsAnEmptySetupToken() {
        val request = mutableListOf<JSONObject>()
        val transport = EnrollmentTransport { _, _, path, body, _ ->
            if (path == "/v1/instance/claim") request += JSONObject(body!!)
            null
        }

        EnrollmentHost(context, MemorySecureStore(), vaultSecrets = VaultSecretStore(MemorySecureStore()), transport = transport).use { host ->
            assertEquals(
                ImportResult.NETWORK_ERROR,
                host.claimOwner("https://enroll.example", "admin", "", "password long enough", generateHostedPassphrase()),
            )
        }

        assertEquals(1, request.size)
        assertFalse(request.single().has("setup_token"))
    }

    @Test fun scannerGateRejectsDuplicateFramesAndLateCallbacks() {
        val gate = EnrollmentScanGate()
        assertTrue(gate.tryStart())
        assertFalse(gate.tryStart())
        gate.cancel()
        assertFalse(gate.tryStart())
        assertFalse(gate.acceptResult())
    }

    @Test fun signInSendsAndroidClientAndPreservesTypedWrongPasswordError() {
        var body: JSONObject? = null
        val transport = EnrollmentTransport { _, _, path, request, _ ->
            if (path == "/v1/instance/session") {
                body = JSONObject(requireNotNull(request))
                EnrollmentHttpResponse(401, "{\"code\":\"invalid_admin_password\"}")
            } else null
        }
        EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
            assertFalse(host.signIn("https://enroll.example", "taylor", "wrong password"))
            assertEquals("android", requireNotNull(body).getString("client"))
            assertEquals("invalid_admin_password", host.takeErrorCode())
        }
    }

    @Test fun signInTreatsNonJsonServerFailureAsUnsuccessful() {
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/session") EnrollmentHttpResponse(502, "<html>bad gateway</html>") else null
        }
        EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
            assertFalse(host.signIn("https://enroll.example", "taylor", "password"))
            assertNull(host.takeErrorCode())
        }
    }

    @Test fun restoredUnacceptedRequestResendsTheExactRustSubmissionBeforePolling() {
        val store = MemorySecureStore()
        val origin = "https://enroll.example"
        val token = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 7 })
        val bodies = mutableListOf<String>()
        var submitAttempts = 0
        val transport = EnrollmentTransport { _, method, path, body, _ ->
            when {
                method == "POST" && path == "/v1/instance/vault-requests" -> {
                    bodies += requireNotNull(body)
                    submitAttempts += 1
                    if (submitAttempts == 1) null else EnrollmentHttpResponse(200, JSONObject().put("request_id", JSONObject(body).getString("request_id")).put("expires_in_seconds", 60).toString())
                }
                method == "GET" -> EnrollmentHttpResponse(200, JSONObject().put("request_id", JSONObject(bodies.last()).getString("request_id")).put("state", "pending").put("expires_in_seconds", 60).toString())
                else -> null
            }
        }
        EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            assertNull(first.beginVaultRequest(origin, "$origin/#invite=$token", "Taylor"))
        }
        EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.VaultRequestState.PENDING, restored.vaultRequestStatus(origin))
        }
        assertEquals(2, bodies.size)
        assertEquals(bodies[0], bodies[1])
    }

    @Test fun phoneNonSuccessStatusRemainsATransientNullResponse() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        val transport = EnrollmentTransport { _, method, path, body, _ ->
            if (path.endsWith("/claim")) EnrollmentHttpResponse(200, fixture.claimResponse(requireNotNull(body)))
            else if (method == "GET") EnrollmentHttpResponse(429, "<html>busy</html>")
            else null
        }
        val flow = requireNotNull(EnrollmentHost(context, MemorySecureStore(), transport = transport).beginPhoneClaim(fixture.qr))
        try { assertNull(flow.status()) } finally { flow.close() }
    }

    @Test fun invalidPhoneQrDoesNotUseTransportAndReportsInvalidQr() {
        var requests = 0
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ ->
            requests += 1
            null
        }).use { host ->
            assertNull(host.beginPhoneClaim("not-a-qr"))
            assertEquals(EnrollmentHost.PhoneClaimFailure.INVALID_QR, host.takePhoneClaimFailure())
            assertNull(host.takePhoneClaimFailure())
        }
        assertEquals(0, requests)
    }

    @Test fun phoneClaimTransportFailureReportsNetwork() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> null }).use { host ->
            assertNull(host.beginPhoneClaim(fixture.qr))
            assertEquals(EnrollmentHost.PhoneClaimFailure.NETWORK, host.takePhoneClaimFailure())
        }
    }

    @Test fun expiredPhoneClaimReportsExpired() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> EnrollmentHttpResponse(410, null) }).use { host ->
            assertNull(host.beginPhoneClaim(fixture.qr))
            assertEquals(EnrollmentHost.PhoneClaimFailure.EXPIRED, host.takePhoneClaimFailure())
        }
    }

    @Test fun rateLimitedPhoneClaimReportsNetwork() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> EnrollmentHttpResponse(429, null) }).use { host ->
            assertNull(host.beginPhoneClaim(fixture.qr))
            assertEquals(EnrollmentHost.PhoneClaimFailure.NETWORK, host.takePhoneClaimFailure())
        }
    }

    @Test fun unavailableInvitationAndInvalidSuccessBodiesHaveSafeFailures() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        val cases = listOf(
            EnrollmentHttpResponse(404, null) to EnrollmentHost.PhoneClaimFailure.EXPIRED,
            EnrollmentHttpResponse(200, null) to EnrollmentHost.PhoneClaimFailure.REJECTED,
            EnrollmentHttpResponse(200, "{}") to EnrollmentHost.PhoneClaimFailure.REJECTED,
        )
        for ((response, expected) in cases) {
            EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> response }).use { host ->
                assertNull(host.beginPhoneClaim(fixture.qr))
                assertEquals(expected, host.takePhoneClaimFailure())
            }
        }
    }

    @Test fun serverFailedPhoneClaimReportsNetwork() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> EnrollmentHttpResponse(503, null) }).use { host ->
            assertNull(host.beginPhoneClaim(fixture.qr))
            assertEquals(EnrollmentHost.PhoneClaimFailure.NETWORK, host.takePhoneClaimFailure())
        }
    }

    @Test fun rejectedPhoneClaimReportsRejected() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        EnrollmentHost(context, MemorySecureStore(), transport = EnrollmentTransport { _, _, _, _, _ -> EnrollmentHttpResponse(401, null) }).use { host ->
            assertNull(host.beginPhoneClaim(fixture.qr))
            assertEquals(EnrollmentHost.PhoneClaimFailure.REJECTED, host.takePhoneClaimFailure())
        }
    }

    @Test fun successfulPhoneClaimHasNoFailure() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        val transport = EnrollmentTransport { _, _, _, body, _ -> EnrollmentHttpResponse(200, fixture.claimResponse(requireNotNull(body))) }
        EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
            requireNotNull(host.beginPhoneClaim(fixture.qr)).close()
            assertNull(host.takePhoneClaimFailure())
        }
    }

    @Test fun phoneClaimFailureIsResetByNextAttempt() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        val transport = EnrollmentTransport { _, _, _, body, _ -> EnrollmentHttpResponse(200, fixture.claimResponse(requireNotNull(body))) }
        EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
            assertNull(host.beginPhoneClaim("not-a-qr"))
            requireNotNull(host.beginPhoneClaim(fixture.qr)).close()
            assertNull(host.takePhoneClaimFailure())
        }
    }

    @Test fun claimedPhoneStatusProducesSasBeforeApprovalAndRetainsSharedSecretMethod() {
        val origin = "https://enroll.example"
        val fixture = EnrollmentNativeFixture(origin)
        val transport = EnrollmentTransport { _, method, path, body, _ ->
            when {
                path.endsWith("/claim") -> EnrollmentHttpResponse(200, fixture.claimResponse(body!!))
                method == "GET" -> EnrollmentHttpResponse(200, fixture.claimedStatus())
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

    @Test fun closingDuringDelayedSharedSecretConfirmationPreventsInstallation() {
        val fixture = EnrollmentNativeFixture("https://enroll.example")
        val confirmStarted = CountDownLatch(1)
        val releaseConfirm = CountDownLatch(1)
        var installs = 0
        val transport = EnrollmentTransport { _, method, path, body, _ ->
            when {
                path.endsWith("/claim") -> EnrollmentHttpResponse(200, fixture.claimResponse(requireNotNull(body)))
                method == "GET" -> EnrollmentHttpResponse(200, fixture.approvedStatus())
                path.endsWith("/confirm") -> {
                    confirmStarted.countDown()
                    check(releaseConfirm.await(2, TimeUnit.SECONDS))
                    EnrollmentHttpResponse(200, "{}")
                }
                else -> null
            }
        }
        val host = EnrollmentHost(context, MemorySecureStore(), { installs += 1; ImportResult.IMPORTED }, transport)
        val executor = Executors.newSingleThreadExecutor()
        try {
            val flow = requireNotNull(host.beginPhoneClaim(fixture.qr))
            val status = requireNotNull(flow.status())
            val confirmation = executor.submit<ImportResult> {
                flow.confirm(status, fixture.passphrase)
            }

            assertTrue(confirmStarted.await(30, TimeUnit.SECONDS))
            flow.close()
            releaseConfirm.countDown()

            assertEquals(ImportResult.SERVER_REJECTED, confirmation.get(2, TimeUnit.SECONDS))
            assertEquals(0, installs)
        } finally {
            releaseConfirm.countDown()
            executor.shutdownNow()
            host.close()
            fixture.close()
        }
    }

    @Test fun ownerClaimWritesPendingSecretBeforeCheckpointAndClaimRequest() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        val pendingKey = VaultSecretStore(store).pendingAccount(origin)
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") store.events += "request:claim"
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
            assertEquals(ImportResult.NETWORK_ERROR, host.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        val pendingWrite = store.events.indexOf("write:$pendingKey")
        val checkpointWrite = store.events.indexOfFirst { it.startsWith("write:$OWNER_CHECKPOINT_PREFIX") }
        val claimRequest = store.events.indexOf("request:claim")
        assertTrue(pendingWrite >= 0)
        assertTrue(checkpointWrite > pendingWrite)
        assertTrue(claimRequest > checkpointWrite)
        assertEquals(secret, String((VaultSecretStore(store).readPending(origin) as StoredSecret.Present).value))
    }

    @Test fun ownerClaimAbortsWithoutRequestWhenPendingSecretCannotBeStored() {
        val store = OrderedSecureStore().apply { failWritePrefix = "vault-secret:pending:" }
        val origin = "https://enroll.example"
        var requests = 0
        val transport = EnrollmentTransport { _, _, _, _, _ ->
            requests += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
            assertEquals(
                ImportResult.STORAGE_ERROR,
                host.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase()),
            )
            assertEquals("secure-store-unavailable", host.takeErrorCode())
        }
        assertEquals(0, requests)
        assertTrue(store.events.none { it.startsWith("write:$OWNER_CHECKPOINT_PREFIX") })
    }

    @Test fun ownerRetryKeepsTheOriginalPendingSecretInStorage() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        val transport = EnrollmentTransport { _, _, _, _, _ -> null }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        val pendingWritesBeforeRetry = store.events.count { it.startsWith("write:vault-secret:pending:") }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(ImportResult.NETWORK_ERROR, restored.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        assertEquals(pendingWritesBeforeRetry, store.events.count { it.startsWith("write:vault-secret:pending:") })
        assertEquals(secret, String((VaultSecretStore(store).readPending(origin) as StoredSecret.Present).value))
    }

    @Test fun inviteApprovedVaultCreationAbortsWithoutRequestWhenPendingSecretCannotBeStored() {
        val store = OrderedSecureStore().apply { failWritePrefix = "vault-secret:pending:" }
        var requests = 0
        val transport = EnrollmentTransport { _, _, _, _, _ ->
            requests += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
            assertEquals(
                ImportResult.STORAGE_ERROR,
                host.createVault("https://enroll.example", "admin", "long-admin-password", generateHostedPassphrase()),
            )
            assertEquals("secure-store-unavailable", host.takeErrorCode())
        }
        assertEquals(0, requests)
    }

    @Test fun resumeWithStoredPendingSecretClaimsWithoutAnEnteredSecret() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val claims = mutableListOf<JSONObject>()
        val transport = EnrollmentTransport { _, _, path, body, _ ->
            if (path == "/v1/instance/claim") claims += JSONObject(body!!)
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase()))
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.STORED, restored.ownerResumeSecret(origin))
            assertEquals(ImportResult.NETWORK_ERROR, restored.claimOwner(origin, "admin", "setup", "password long enough", ""))
        }
        assertEquals(2, claims.size)
        assertEquals(claims[0].getString("operation_id"), claims[1].getString("operation_id"))
    }

    @Test fun storedPendingSecretCompletesAResumedOwnerClaimWithoutAnEnteredSecret() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        var claimAttempts = 0
        var installed = 0
        val transport = EnrollmentTransport { _, _, path, body, _ ->
            if (path != "/v1/instance/claim") {
                null
            } else {
                claimAttempts += 1
                if (claimAttempts == 1) {
                    null
                } else {
                    val claim = JSONObject(requireNotNull(body))
                    val profile = JSONObject(claim.getJSONObject("material").getString("profile"))
                    EnrollmentHttpResponse(
                        200,
                        JSONObject()
                            .put("vault_id", profile.getString("vault_id"))
                            .put("device_id", claim.getJSONObject("device").getString("device_id"))
                            .put("replayed", false)
                            .put("session", JSONObject().put("session_token", "session").put("expires_in_seconds", 60))
                            .toString(),
                    )
                }
            }
        }
        val secret = generateHostedPassphrase()
        EnrollmentHost(context, checkpointStore = store, identityInstaller = { installed += 1; ImportResult.IMPORTED }, transport = transport, vaultSecrets = VaultSecretStore(store)).use { first ->
            assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        EnrollmentHost(context, checkpointStore = store, identityInstaller = { installed += 1; ImportResult.IMPORTED }, transport = transport, vaultSecrets = VaultSecretStore(store)).use { restored ->
            assertEquals(ImportResult.IMPORTED, restored.claimOwner(origin, "admin", "setup", "password long enough", ""))
        }
        assertEquals(2, claimAttempts)
        assertEquals(1, installed)
    }

    @Test fun resumeWithoutStoredPendingSecretPromptsAndRejectsBlankEnteredSecret() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        var claims = 0
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") claims += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase())
        }
        store.values.remove(VaultSecretStore(store).pendingAccount(origin))
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.PROMPT, restored.ownerResumeSecret(origin))
            assertEquals(ImportResult.SERVER_REJECTED, restored.claimOwner(origin, "admin", "setup", "password long enough", ""))
            assertEquals("owner-secret-required", restored.takeErrorCode())
        }
        assertEquals(1, claims)
    }

    @Test fun legacyResumeWithCorrectEnteredSecretStoresPendingBeforeClaiming() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        var claims = 0
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") claims += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.claimOwner(origin, "admin", "setup", "password long enough", secret)
        }
        store.values.remove(VaultSecretStore(store).pendingAccount(origin))
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(ImportResult.NETWORK_ERROR, restored.claimOwner(origin, "admin", "setup", "password long enough", secret))
        }
        assertEquals(2, claims)
        assertEquals(secret, String((VaultSecretStore(store).readPending(origin) as StoredSecret.Present).value))
    }

    @Test fun legacyResumeRejectsWrongEnteredSecretWithoutClaimingOrStoringIt() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        var claims = 0
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") claims += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase())
        }
        store.values.remove(VaultSecretStore(store).pendingAccount(origin))
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(
                ImportResult.SERVER_REJECTED,
                restored.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase()),
            )
            assertEquals("wrong-passphrase", restored.takeErrorCode())
        }
        assertEquals(1, claims)
        assertNull(store.values[VaultSecretStore(store).pendingAccount(origin)])
    }

    @Test fun storedPendingSecretThatDoesNotMatchCheckpointIsRejectedWithoutClaiming() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        var claims = 0
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") claims += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase())
        }
        store.values[VaultSecretStore(store).pendingAccount(origin)] = generateHostedPassphrase().toByteArray(Charsets.UTF_8)
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(ImportResult.SERVER_REJECTED, restored.claimOwner(origin, "admin", "setup", "password long enough", ""))
            assertEquals("vault-secret-mismatch", restored.takeErrorCode())
        }
        assertEquals(1, claims)
    }

    @Test fun pendingReadFailureFailsClosedWithoutPromptingOrRegenerating() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        var claims = 0
        val transport = EnrollmentTransport { _, _, path, _, _ ->
            if (path == "/v1/instance/claim") claims += 1
            null
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.claimOwner(origin, "admin", "setup", "password long enough", generateHostedPassphrase())
        }
        store.failReadPrefix = "vault-secret:pending:"
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.UNAVAILABLE, restored.ownerResumeSecret(origin))
            assertEquals(ImportResult.STORAGE_ERROR, restored.claimOwner(origin, "admin", "setup", "password long enough", ""))
            assertEquals("secure-store-unavailable", restored.takeErrorCode())
        }
        assertEquals(1, claims)
    }

    @Test fun approvedVaultCreationResumesWithStoredPendingSecretWithoutEnteredSecret() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        val creates = mutableListOf<JSONObject>()
        val transport = approvedVaultRequestTransport(creates)
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            assertEquals(EnrollmentHost.VaultRequestState.PENDING, first.beginVaultRequest(origin, "$origin/#invite=$INVITE_TOKEN", "Taylor"))
            assertEquals(EnrollmentHost.VaultRequestState.APPROVED, first.vaultRequestStatus(origin))
            assertEquals(ImportResult.NETWORK_ERROR, first.createVault(origin, "taylor", "long-admin-password", secret))
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.STORED, restored.vaultCreationSecret(origin))
            assertEquals(ImportResult.NETWORK_ERROR, restored.createVault(origin, "taylor", "long-admin-password", ""))
        }
        assertEquals(2, creates.size)
        assertEquals(creates[0].getString("operation_id"), creates[1].getString("operation_id"))
    }

    @Test fun approvedVaultCreationPromptsAndStoresEnteredSecretBeforeRequestWhenPendingIsMissing() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val secret = generateHostedPassphrase()
        val pendingKey = VaultSecretStore(store).pendingAccount(origin)
        val pendingAtRequest = mutableListOf<String?>()
        val creates = mutableListOf<JSONObject>()
        val transport = approvedVaultRequestTransport(creates) {
            pendingAtRequest += store.values[pendingKey]?.toString(Charsets.UTF_8)
        }
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.beginVaultRequest(origin, "$origin/#invite=$INVITE_TOKEN", "Taylor")
            first.vaultRequestStatus(origin)
            assertEquals(ImportResult.NETWORK_ERROR, first.createVault(origin, "taylor", "long-admin-password", secret))
        }
        store.values.remove(pendingKey)
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.PROMPT, restored.vaultCreationSecret(origin))
            assertEquals(ImportResult.SERVER_REJECTED, restored.createVault(origin, "taylor", "long-admin-password", ""))
            assertEquals("owner-secret-required", restored.takeErrorCode())
            assertEquals(ImportResult.NETWORK_ERROR, restored.createVault(origin, "taylor", "long-admin-password", secret))
        }
        assertEquals(2, creates.size)
        assertEquals(listOf(secret, secret), pendingAtRequest)
        assertEquals(secret, String((VaultSecretStore(store).readPending(origin) as StoredSecret.Present).value))
    }

    @Test fun approvedVaultCreationPendingReadFailureFailsClosedWithoutRequest() {
        val store = OrderedSecureStore()
        val origin = "https://enroll.example"
        val creates = mutableListOf<JSONObject>()
        val transport = approvedVaultRequestTransport(creates)
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
            first.beginVaultRequest(origin, "$origin/#invite=$INVITE_TOKEN", "Taylor")
            first.vaultRequestStatus(origin)
            first.createVault(origin, "taylor", "long-admin-password", generateHostedPassphrase())
        }
        store.failReadPrefix = "vault-secret:pending:"
        EnrollmentHost(context, checkpointStore = store, vaultSecrets = VaultSecretStore(store), transport = transport).use { restored ->
            assertEquals(EnrollmentHost.ResumeSecret.UNAVAILABLE, restored.vaultCreationSecret(origin))
            assertEquals(ImportResult.STORAGE_ERROR, restored.createVault(origin, "taylor", "long-admin-password", ""))
            assertEquals("secure-store-unavailable", restored.takeErrorCode())
        }
        assertEquals(1, creates.size)
    }

    private fun approvedVaultRequestTransport(
        creates: MutableList<JSONObject>,
        onVaultCreate: () -> Unit = {},
    ): EnrollmentTransport {
        var requestId: String? = null
        return EnrollmentTransport { _, method, path, body, _ ->
            when {
                method == "POST" && path == "/v1/instance/vault-requests" -> {
                    requestId = JSONObject(requireNotNull(body)).getString("request_id")
                    EnrollmentHttpResponse(200, JSONObject().put("request_id", requestId).put("expires_in_seconds", 60).toString())
                }
                method == "GET" && path.startsWith("/v1/instance/vault-requests/") ->
                    EnrollmentHttpResponse(200, JSONObject().put("request_id", requestId).put("state", "approved").put("expires_in_seconds", 60).toString())
                path == "/v1/instance/vaults" -> {
                    creates += JSONObject(requireNotNull(body))
                    onVaultCreate()
                    null
                }
                else -> null
            }
        }
    }

    private class EnrollmentNativeFixture(private val origin: String) : AutoCloseable {
        private val invitationId = "00000000-0000-0000-0000-000000000007"
        private val invitationToken = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 2 })
        private val ownerTransferKey = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 3 })
        val passphrase = generateHostedPassphrase()
        private val material: JSONObject
        private lateinit var claimRequest: JSONObject
        val qr: String = JSONObject()
            .put("version", 1)
            .put("purpose", "connect_phone")
            .put("origin", origin)
            .put("invitation_token", invitationToken)
            .put("transfer_public_key", ownerTransferKey)
            .toString()

        init {
            val owner = newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "fixture owner")
            try {
                material = JSONObject(owner.createOwnerRequest("operator-code", "fixture", "long-admin-password", passphrase))
                    .getJSONObject("material")
            } finally {
                owner.close()
            }
        }

        fun claimResponse(body: String): String {
            claimRequest = JSONObject(body)
            return JSONObject()
                .put("invitation_id", invitationId)
                .put("claim_secret", java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 4 }))
                .put("origin", origin)
                .put("vault_id", JSONObject(material.getString("profile")).getString("vault_id"))
                .put("fingerprint", material.getString("fingerprint"))
                .put("key_epoch", material.getInt("key_epoch"))
                .put("method", "shared_secret")
                .put("owner_transfer_public_key", ownerTransferKey)
                .put("expires_in_seconds", 300)
                .toString()
        }

        fun claimedStatus(): String {
            val device = claimRequest.getJSONObject("device")
            val transcript = JSONObject()
                .put("origin", origin)
                .put("vault_id", JSONObject(material.getString("profile")).getString("vault_id"))
                .put("fingerprint", material.getString("fingerprint"))
                .put("key_epoch", material.getInt("key_epoch"))
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
                .put("profile", material.get("profile"))
                .toString()
        }

        fun approvedStatus(): String = JSONObject(claimedStatus())
            .put("state", "approved")
            .toString()

        override fun close() = Unit
    }

    private class MemorySecureStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        override fun read(key: String) = values[key]?.copyOf()
        override fun write(key: String, value: ByteArray) { values[key] = value.copyOf() }
        override fun remove(key: String) { values.remove(key) }
    }

    private class OrderedSecureStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        val events = mutableListOf<String>()
        var failWritePrefix: String? = null
        var failReadPrefix: String? = null
        override fun read(key: String): ByteArray? {
            if (failReadPrefix?.let { key.startsWith(it) } == true) throw IllegalStateException("unavailable")
            return values[key]?.copyOf()
        }
        override fun write(key: String, value: ByteArray) {
            if (failWritePrefix?.let { key.startsWith(it) } == true) throw IllegalStateException("unavailable")
            events += "write:$key"
            values[key] = value.copyOf()
        }
        override fun remove(key: String) { values.remove(key) }
    }

    private companion object {
        const val OWNER_CHECKPOINT_PREFIX = "self-hosted-owner-enrollment-checkpoint-v1-"
        val INVITE_TOKEN: String = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 5 })
    }
}
