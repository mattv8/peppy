package dev.peppy.mobile.enrollment

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.account.HostedSecureStore
import dev.peppy.mobile.gateway.ImportResult
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.gateway.TestKeyHolder
import dev.peppy.mobile.security.StoredSecret
import dev.peppy.mobile.security.VaultSecretStore
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeEnrollmentClientKind
import uniffi.peppy_mobile_bindings.NativeEnrollmentMethod
import uniffi.peppy_mobile_bindings.generateHostedPassphrase
import uniffi.peppy_mobile_bindings.newNativeEnrollmentClient

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class NativeEnrollmentInstallTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val keys = TestKeyHolder()
    private val originalSecretBox = NativeGateway.secretBox

    @Before
    fun setUp() {
        NativeGateway.closeForTest()
        NativeGateway.secretBox = keys.box
        context.getSharedPreferences("peppy-secure", Context.MODE_PRIVATE).edit().clear().commit()
        NativeGateway.databaseFile(context).delete()
    }

    @After
    fun tearDown() {
        NativeGateway.closeForTest()
        NativeGateway.secretBox = originalSecretBox
        context.getSharedPreferences("peppy-secure", Context.MODE_PRIVATE).edit().clear().commit()
        NativeGateway.databaseFile(context).delete()
    }

    @Test
    fun nativeOwnerIdentityInstallsUnlocksAndReopensFromTheRealKeyCache() {
        NativeEnrollmentFixture("https://native-enrollment.example", "fixture shared vault passphrase").use { fixture ->
            assertEquals(
                ImportResult.IMPORTED,
                NativeGateway.installPhraseEnrollmentIdentity(context, fixture.ownerIdentity(), fixture.passphrase),
            )
        }

        assertReopensWithReadyKeys()
    }

    @Test
    fun freshOwnerClaimUsesTheProductionPhraseInstaller() {
        val origin = "https://native-owner.example"
        val store = MemorySecureStore()
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            val transport = EnrollmentTransport { _, _, path, body, _ ->
                if (path == "/v1/instance/claim") {
                    EnrollmentHttpResponse(200, fixture.claimResponse(JSONObject(requireNotNull(body))).toString())
                } else {
                    null
                }
            }
            EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                assertEquals(ImportResult.IMPORTED, host.claimOwner(origin, "admin", "setup", "password long enough", fixture.passphrase))
            }
        }
        assertReopensWithReadyKeys()
    }

    @Test
    fun checkpointResumedOwnerClaimUsesStoredPhraseAndProductionInstaller() {
        val origin = "https://native-resumed-owner.example"
        val store = MemorySecureStore()
        var attempts = 0
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            val transport = EnrollmentTransport { _, _, path, body, _ ->
                if (path != "/v1/instance/claim") {
                    null
                } else {
                    attempts += 1
                    if (attempts == 1) null else EnrollmentHttpResponse(
                        200,
                        fixture.claimResponse(JSONObject(requireNotNull(body))).toString(),
                    )
                }
            }
            EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { first ->
                assertEquals(ImportResult.NETWORK_ERROR, first.claimOwner(origin, "admin", "setup", "password long enough", fixture.passphrase))
                assertTrue(first.hasPendingOwner(origin))
            }
            EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { resumed ->
                assertEquals(EnrollmentHost.ResumeSecret.STORED, resumed.ownerResumeSecret(origin))
                assertEquals(ImportResult.IMPORTED, resumed.claimOwner(origin, "admin", "setup", "password long enough", ""))
                assertFalse(resumed.hasPendingOwner(origin))
            }
        }
        assertEquals(2, attempts)
        assertReopensWithReadyKeys()
    }

    @Test
    fun manualAdmissionUsesGeneratedMaterialAndTheProductionPhraseInstaller() {
        val origin = "https://native-manual.example"
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            var transportFailure: Exception? = null
            val transport = EnrollmentTransport { _, _, path, body, _ ->
                try {
                    when {
                        path == "/v1/enrollment/challenges" -> EnrollmentHttpResponse(200, fixture.manualChallenge(requireNotNull(body)).toString())
                        path.endsWith("/complete") -> EnrollmentHttpResponse(200, fixture.admissionComplete(requireNotNull(body)).toString())
                        else -> null
                    }
                } catch (failure: Exception) {
                    transportFailure = failure
                    throw failure
                }
            }
            EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
                val result = host.joinWithSecret(origin, null, fixture.passphrase)
                assertNull("the binding-generated completion request must be accepted by the HTTP fixture", transportFailure)
                assertEquals(ImportResult.IMPORTED, result)
            }
        }
        assertNotNull(NativeGateway.session(context))
    }

    @Test
    fun approvedVaultRequestUsesTheProductionPhraseInstaller() {
        val origin = "https://native-vault-request.example"
        val invite = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { 3 })
        val store = MemorySecureStore()
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            var requestId: String? = null
            val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                method == "POST" && path == "/v1/instance/vault-requests" -> JSONObject(requireNotNull(body)).let {
                    requestId = it.getString("request_id")
                    EnrollmentHttpResponse(200, JSONObject().put("request_id", requestId).put("expires_in_seconds", 300).toString())
                }
                method == "GET" && path.startsWith("/v1/instance/vault-requests/") -> EnrollmentHttpResponse(200, JSONObject().put("request_id", requestId).put("state", "approved").put("expires_in_seconds", 300).toString())
                method == "POST" && path == "/v1/instance/vaults" -> EnrollmentHttpResponse(200, fixture.claimResponse(JSONObject(requireNotNull(body))).toString())
                else -> null
            } }
            EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                assertEquals(EnrollmentHost.VaultRequestState.PENDING, host.beginVaultRequest(origin, "$origin/#invite=$invite", "fixture"))
                assertEquals(EnrollmentHost.VaultRequestState.APPROVED, host.vaultRequestStatus(origin))
                assertEquals(ImportResult.IMPORTED, host.createVault(origin, "admin", "password long enough", fixture.passphrase))
            }
        }
        assertNotNull(NativeGateway.session(context))
    }

    @Test
    fun sharedPhoneAdmissionUsesTheProductionPhraseInstallerAndReopens() {
        phoneAdmission(NativeEnrollmentMethod.SHARED_SECRET)
    }

    @Test
    fun transferPhoneAdmissionUsesTheProductionHandoffInstallerAndReopens() {
        phoneAdmission(NativeEnrollmentMethod.TRANSFER)
    }

    @Test
    fun cachedPhoneRetryUsesOriginalPhraseWithNilAndChangedArguments() {
        for (retrySecret in listOf<String?>(null, generateHostedPassphrase())) {
            resetGatewayState()
            val origin = "https://native-phone-retry-${retrySecret == null}.example"
            val store = MemorySecureStore()
            NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
                fixture.phoneInvitation(NativeEnrollmentMethod.SHARED_SECRET).use { phone ->
                    val bodies = mutableListOf<String>()
                    var attempts = 0
                    val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                        path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                        method == "POST" && path.endsWith("/confirm") -> {
                            bodies += requireNotNull(body)
                            attempts += 1
                            if (attempts == 1) null else EnrollmentHttpResponse(200, phone.admissionComplete())
                        }
                        else -> null
                    } }
                    EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                        val flow = requireNotNull(host.beginPhoneClaim(phone.qrPayload))
                        phone.approve()
                        val approved = phone.status()
                        assertEquals(ImportResult.NETWORK_ERROR, flow.confirm(approved, fixture.passphrase))
                        assertEquals(ImportResult.IMPORTED, flow.confirm(JSONObject(approved).put("state", "consumed").toString(), retrySecret))
                    }
                    assertEquals(bodies[0], bodies[1])
                    val credential = requireNotNull(NativeGateway.accountCredential(context))
                    assertStoredPhrase(store, credential.vaultId, credential.deviceId, fixture.passphrase)
                }
            }
            assertReopensWithReadyKeys()
        }
    }

    @Test
    fun definitiveProofMismatchAcceptsACorrectedPhrase() {
        val origin = "https://native-phone-corrected.example"
        val store = MemorySecureStore()
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            fixture.phoneInvitation(NativeEnrollmentMethod.SHARED_SECRET).use { phone ->
                val bodies = mutableListOf<String>()
                var attempts = 0
                val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                    path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                    method == "POST" && path.endsWith("/confirm") -> {
                        bodies += requireNotNull(body)
                        attempts += 1
                        if (attempts == 1) {
                            EnrollmentHttpResponse(403, "{\"code\":\"invitation_proof_mismatch\"}")
                        } else {
                            EnrollmentHttpResponse(200, phone.admissionComplete())
                        }
                    }
                    else -> null
                } }
                EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                    val flow = requireNotNull(host.beginPhoneClaim(phone.qrPayload))
                    phone.approve()
                    val approved = phone.status()
                    assertEquals(ImportResult.SERVER_REJECTED, flow.confirm(approved, generateHostedPassphrase()))
                    assertTrue(flow.needsCorrectedPhrase())
                    assertEquals(ImportResult.IMPORTED, flow.confirm(approved, fixture.passphrase))
                }
                assertNotEquals(bodies[0], bodies[1])
                val credential = requireNotNull(NativeGateway.accountCredential(context))
                assertStoredPhrase(store, credential.vaultId, credential.deviceId, fixture.passphrase)
            }
        }
        assertReopensWithReadyKeys()
    }

    @Test
    fun closeReturnsWhileConfirmationHttpIsHeldAndRetiredFlowCannotInstall() {
        val origin = "https://native-phone-retired.example"
        val store = MemorySecureStore()
        val confirmStarted = CountDownLatch(1)
        val releaseConfirm = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(2)
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            fixture.phoneInvitation(NativeEnrollmentMethod.SHARED_SECRET).use { phone ->
                val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                    path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                    method == "POST" && path.endsWith("/confirm") -> {
                        confirmStarted.countDown()
                        check(releaseConfirm.await(30, TimeUnit.SECONDS))
                        EnrollmentHttpResponse(200, phone.admissionComplete())
                    }
                    else -> null
                } }
                EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                    val flow = requireNotNull(host.beginPhoneClaim(phone.qrPayload))
                    phone.approve()
                    val confirmation = executor.submit<ImportResult> { flow.confirm(phone.status(), fixture.passphrase) }
                    assertTrue(confirmStarted.await(30, TimeUnit.SECONDS))

                    val close = executor.submit { flow.close() }
                    close.get(1, TimeUnit.SECONDS)
                    releaseConfirm.countDown()

                    assertEquals(ImportResult.SERVER_REJECTED, confirmation.get(30, TimeUnit.SECONDS))
                }
            }
        }
        executor.shutdownNow()
        assertNull(NativeGateway.accountCredential(context))
        assertNull(NativeGateway.session(context))
        val preferences = context.getSharedPreferences("peppy-secure", Context.MODE_PRIVATE)
        assertFalse(preferences.contains("token.v1"))
        assertFalse(preferences.contains("key-cache.v1"))
        assertTrue(store.values.keys.none { it.startsWith("vault-secret:v2:") })
    }

    @Test
    fun duplicateConfirmationIsRejectedWithoutASecondHttpRequest() {
        val origin = "https://native-phone-single-flight.example"
        val confirmStarted = CountDownLatch(1)
        val releaseConfirm = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(2)
        var confirms = 0
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            fixture.phoneInvitation(NativeEnrollmentMethod.SHARED_SECRET).use { phone ->
                val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                    path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                    method == "POST" && path.endsWith("/confirm") -> {
                        confirms += 1
                        confirmStarted.countDown()
                        check(releaseConfirm.await(30, TimeUnit.SECONDS))
                        EnrollmentHttpResponse(200, phone.admissionComplete())
                    }
                    else -> null
                } }
                EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
                    val flow = requireNotNull(host.beginPhoneClaim(phone.qrPayload))
                    phone.approve()
                    val approved = phone.status()
                    val first = executor.submit<ImportResult> { flow.confirm(approved, fixture.passphrase) }
                    assertTrue(confirmStarted.await(30, TimeUnit.SECONDS))
                    val duplicate = executor.submit<ImportResult> { flow.confirm(approved, fixture.passphrase) }
                    assertEquals(ImportResult.SERVER_REJECTED, duplicate.get(1, TimeUnit.SECONDS))
                    assertEquals(1, confirms)
                    releaseConfirm.countDown()
                    assertEquals(ImportResult.IMPORTED, first.get(30, TimeUnit.SECONDS))
                }
            }
        }
        executor.shutdownNow()
        assertReopensWithReadyKeys()
    }

    @Test
    fun bindingSecretWriteFailureDoesNotRollbackInstalledCache() {
        val origin = "https://native-phone-binding-write-failure.example"
        val store = MemorySecureStore().apply { failWritePrefix = "vault-secret:v2:" }
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            fixture.phoneInvitation(NativeEnrollmentMethod.SHARED_SECRET).use { phone ->
                val transport = EnrollmentTransport { _, method, path, body, _ -> when {
                    path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                    method == "POST" && path.endsWith("/confirm") -> EnrollmentHttpResponse(200, phone.admissionComplete())
                    else -> null
                } }
                EnrollmentHost(context, store, vaultSecrets = VaultSecretStore(store), transport = transport).use { host ->
                    val flow = requireNotNull(host.beginPhoneClaim(phone.qrPayload))
                    phone.approve()
                    assertEquals(ImportResult.IMPORTED, flow.confirm(phone.status(), fixture.passphrase))
                }
            }
        }
        val credential = requireNotNull(NativeGateway.accountCredential(context))
        assertEquals(StoredSecret.Missing, VaultSecretStore(store).read(credential.vaultId, credential.deviceId))
        assertReopensWithReadyKeys()
    }

    private fun phoneAdmission(method: NativeEnrollmentMethod) {
        val methodSlug = method.name.lowercase().replace('_', '-')
        val origin = "https://native-phone-$methodSlug.example"
        NativeEnrollmentFixture(origin, "fixture shared vault passphrase").use { fixture ->
            fixture.phoneInvitation(method).use { phone ->
                assertNull("the native binding must accept the generated phone QR", directPhoneClaimFailure(phone))
                val transport = EnrollmentTransport { _, httpMethod, path, body, _ -> when {
                    path.endsWith("/claim") -> EnrollmentHttpResponse(200, phone.consumeClaim(requireNotNull(body)))
                    httpMethod == "GET" && path.startsWith("/v1/enrollment/invitations/") -> EnrollmentHttpResponse(200, phone.status())
                    httpMethod == "POST" && path.endsWith("/confirm") -> EnrollmentHttpResponse(200, phone.admissionComplete())
                    else -> null
                } }
                EnrollmentHost(context, MemorySecureStore(), transport = transport).use { host ->
                    val flow = host.beginPhoneClaim(phone.qrPayload)
                    assertNotNull("phone claim failed as ${host.takePhoneClaimFailure()}", flow)
                    requireNotNull(flow)
                    try {
                        phone.approve()
                        val status = requireNotNull(flow.status())
                        assertEquals(ImportResult.IMPORTED, flow.confirm(status, if (method == NativeEnrollmentMethod.SHARED_SECRET) fixture.passphrase else null))
                    } finally {
                        flow.close()
                    }
                }
            }
        }
        assertReopensWithReadyKeys()
    }

    private fun assertReopensWithReadyKeys() {
        assertNotNull(NativeGateway.session(context))
        NativeGateway.closeForTest()
        assertNotNull("a new native client opens the retained encrypted database", NativeGateway.open(context))
        assertNotNull("the reopened client imports the protected native key cache", NativeGateway.session(context))
    }

    private fun resetGatewayState() {
        NativeGateway.closeForTest()
        context.getSharedPreferences("peppy-secure", Context.MODE_PRIVATE).edit().clear().commit()
        NativeGateway.databaseFile(context).delete()
    }

    private fun assertStoredPhrase(store: MemorySecureStore, vaultId: String, deviceId: String, expected: String) {
        val stored = VaultSecretStore(store).read(vaultId, deviceId)
        assertTrue(stored is StoredSecret.Present)
        stored as StoredSecret.Present
        try {
            assertEquals(expected, String(stored.value))
        } finally {
            stored.value.fill('\u0000')
        }
    }

    private fun directPhoneClaimFailure(phone: NativeEnrollmentFixture.PhoneInvitationFixture): Exception? = try {
        val origin = JSONObject(phone.qrPayload).getString("origin")
        newNativeEnrollmentClient(origin, NativeEnrollmentClientKind.ANDROID, "QR validation fixture").use { client ->
            val request = client.claimPhoneInvitationRequest(phone.qrPayload)
            client.consumePhoneClaim(phone.consumeClaim(request))
        }
        null
    } catch (failure: Exception) {
        failure
    }

    private class MemorySecureStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        var failWritePrefix: String? = null

        override fun read(key: String): ByteArray? = values[key]?.copyOf()

        override fun write(key: String, value: ByteArray) {
            if (failWritePrefix?.let(key::startsWith) == true) throw IllegalStateException("unavailable")
            values[key] = value.copyOf()
        }

        override fun remove(key: String) {
            values.remove(key)
        }
    }
}
