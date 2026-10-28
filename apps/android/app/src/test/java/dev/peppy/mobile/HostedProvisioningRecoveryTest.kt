package dev.peppy.mobile

import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeHostedClientKind
import uniffi.peppy_mobile_bindings.prepareHostedProvisioning
import java.io.IOException
import java.util.Base64
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class HostedProvisioningRecoveryTest : GatewayTestBase() {
    private class Store : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        var failBindingWrite = false
        var corruptBindingReadback = false
        var failReadKey: String? = null
        private var bindingWritten = false
        var replaceCheckpointOnRead = false
        private var checkpointReads = 0

        override fun read(key: String): ByteArray? = synchronized(values) {
            if (key == failReadKey) throw IOException("secure store unavailable")
            if (corruptBindingReadback && bindingWritten && key.startsWith("vault-secret:v2:")) {
                corruptBindingReadback = false
                return "corrupt readback".toByteArray()
            }
            if (key == HostedAccountClient.CHECKPOINT_KEY && replaceCheckpointOnRead && ++checkpointReads == 3) {
                values[key] = "replacement".toByteArray()
            }
            values[key]?.clone()
        }

        override fun write(key: String, value: ByteArray) = synchronized(values) {
            if (failBindingWrite && key.startsWith("vault-secret:v2:")) throw IOException("binding write failed")
            if (key.startsWith("vault-secret:v2:")) bindingWritten = true
            values[key] = value.clone()
        }

        override fun remove(key: String) {
            synchronized(values) { values.remove(key) }
        }
    }

    private data class Prepared(
        val accountId: String,
        val vaultId: String,
        val deviceId: String,
        val checkpoint: ByteArray,
        val material: VaultMaterial,
    )

    private val phrase = "velvet orchard lantern canyon glacier silver"
    private val store = Store()

    @Before
    fun initializeWorkManager() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun resetHostedStore() {
        HostedProvisioningStorage.resetForTest()
    }

    @Test
    fun verifiedManualUnlockStoresTheExactPhraseButWrongPhraseDoesNot() = runBlocking {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared)
        enroll(prepared)

        assertEquals(UnlockResult.WRONG_PASSPHRASE, NativeGateway.unlock(context, "wrong hosted phrase"))
        assertNull(binding(prepared))

        assertEquals(UnlockResult.UNLOCKED, NativeGateway.unlock(context, phrase))
        assertEquals(phrase, binding(prepared))
        assertTrue(store.values.containsKey(HostedAccountClient.CHECKPOINT_KEY))

        val revealed = revealOwnerVaultSecret(
            context,
            DeviceAuthenticator { DeviceAuthOutcome.AUTHENTICATED },
        ) as RevealOutcome.Revealed
        revealed.secret.fill('\u0000')
        assertNull(store.values[pendingKey(prepared)])
        assertNull(store.values[HostedAccountClient.CHECKPOINT_KEY])
    }

    @Test
    fun authenticatedRevealRepairsAfterImportWithoutSessionOrEpochActivation() = runBlocking {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared)
        enroll(prepared)
        assertFalse(NativeGateway.status(context).sharedKeysReady)

        val outcome = revealOwnerVaultSecret(context, DeviceAuthenticator { DeviceAuthOutcome.AUTHENTICATED })
        assertTrue(outcome is RevealOutcome.Revealed)
        val revealed = (outcome as RevealOutcome.Revealed).secret
        try {
            assertEquals(phrase, String(revealed))
        } finally {
            revealed.fill('\u0000')
        }
        assertFalse("pure recovery must not unlock or activate the core", NativeGateway.status(context).sharedKeysReady)
        assertEquals(phrase, binding(prepared))
        assertNull(store.values[pendingKey(prepared)])
        assertNull(store.values[HostedAccountClient.CHECKPOINT_KEY])
        assertNull("recovery must not require an account session", store.values[HostedAccountClient.SESSION_KEY])
    }

    @Test
    fun wrongPendingConflictingBindingAndReadFailureRetainRecoveryMaterial() {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared, "wrong pending phrase")
        enroll(prepared)
        val checkpoint = store.values.getValue(HostedAccountClient.CHECKPOINT_KEY).clone()

        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertArrayEquals(checkpoint, store.values[HostedAccountClient.CHECKPOINT_KEY])
        assertEquals("wrong pending phrase", store.values[pendingKey(prepared)]?.toString(Charsets.UTF_8))

        store.values[pendingKey(prepared)] = phrase.toByteArray()
        store.values[bindingKey(prepared)] = "conflicting binding".toByteArray()
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertEquals("conflicting binding", binding(prepared))
        assertArrayEquals(checkpoint, store.values[HostedAccountClient.CHECKPOINT_KEY])

        store.values.remove(bindingKey(prepared))
        store.failReadKey = pendingKey(prepared)
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertArrayEquals(checkpoint, store.values[HostedAccountClient.CHECKPOINT_KEY])
    }

    @Test
    fun failedBindingWriteAndConcurrentCheckpointReplacementAreRetainedForRetry() {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared)
        enroll(prepared)

        store.failBindingWrite = true
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertNull(binding(prepared))
        assertTrue(store.values.containsKey(pendingKey(prepared)))
        assertTrue(store.values.containsKey(HostedAccountClient.CHECKPOINT_KEY))

        store.failBindingWrite = false
        store.replaceCheckpointOnRead = true
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertEquals("replacement", store.values[HostedAccountClient.CHECKPOINT_KEY]?.toString(Charsets.UTF_8))
        assertTrue(store.values.containsKey(pendingKey(prepared)))
    }

    @Test
    fun failedBindingReadbackRetainsRecoveryAndTheNextAttemptRepairsIt() {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared)
        enroll(prepared)

        store.corruptBindingReadback = true
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertEquals(phrase, binding(prepared))
        assertTrue(store.values.containsKey(pendingKey(prepared)))
        assertTrue(store.values.containsKey(HostedAccountClient.CHECKPOINT_KEY))

        assertTrue(NativeGateway.repairHostedVaultSecret(context))
        assertNull(store.values[pendingKey(prepared)])
        assertNull(store.values[HostedAccountClient.CHECKPOINT_KEY])
    }

    @Test
    fun wrongIdentityTupleAndMalformedCheckpointChangeNothing() {
        HostedProvisioningStorage.storeFactory = { store }
        val prepared = prepared()
        seedRecovery(prepared)
        assertEquals(
            ImportResult.IMPORTED,
            NativeGateway.persistVerified(
                context,
                parsed(origin = HostedAccountClient.ORIGIN),
                vaultMaterial(),
            ),
        )
        val checkpoint = store.values.getValue(HostedAccountClient.CHECKPOINT_KEY).clone()
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertArrayEquals(checkpoint, store.values[HostedAccountClient.CHECKPOINT_KEY])

        store.values[HostedAccountClient.CHECKPOINT_KEY] = "malformed".toByteArray()
        assertFalse(NativeGateway.repairHostedVaultSecret(context))
        assertEquals("malformed", store.values[HostedAccountClient.CHECKPOINT_KEY]?.toString(Charsets.UTF_8))
    }

    private fun prepared(): Prepared {
        val accountId = UUID.randomUUID().toString()
        return prepareHostedProvisioning(
            HostedAccountClient.ORIGIN,
            accountId,
            phrase,
            NativeHostedClientKind.ANDROID,
            "Android phone",
        ).use { provisioning ->
            provisioning.acceptGrant("""{"grant":"pgr_${"a".repeat(64)}","expires_in_seconds":600}""")
            val view = provisioning.view()
            val request = JSONObject(provisioning.completeRequest())
            val header = String(
                Base64.getDecoder().decode(request.getString("encrypted_vault_check_header")),
                Charsets.UTF_8,
            )
            Prepared(
                accountId,
                view.vaultId,
                view.deviceId,
                provisioning.checkpoint(),
                VaultMaterial(request.getJSONObject("public_key_profile").toString(), header),
            )
        }
    }

    private fun enroll(prepared: Prepared) {
        assertEquals(
            ImportResult.IMPORTED,
            NativeGateway.persistVerified(
                context,
                parsed(
                    origin = HostedAccountClient.ORIGIN,
                    vault = prepared.vaultId,
                    device = prepared.deviceId,
                ),
                prepared.material,
            ),
        )
    }

    private fun seedRecovery(prepared: Prepared, pending: String = phrase) {
        store.values[HostedAccountClient.CHECKPOINT_KEY] = prepared.checkpoint.clone()
        store.values[pendingKey(prepared)] = pending.toByteArray()
    }

    private fun pendingKey(prepared: Prepared) =
        VaultSecretStore(store).pendingAccount(HostedAccountClient.ORIGIN, prepared.accountId)

    private fun bindingKey(prepared: Prepared) = VaultSecretStore(store).account(prepared.vaultId, prepared.deviceId)

    private fun binding(prepared: Prepared) = store.values[bindingKey(prepared)]?.toString(Charsets.UTF_8)
}
