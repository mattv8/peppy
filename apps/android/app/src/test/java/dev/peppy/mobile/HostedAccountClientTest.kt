package dev.peppy.mobile

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.generateHostedPassphrase
import java.io.IOException
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class HostedAccountClientTest {
    private class Store : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        var failWrite: String? = null
        var failRead: String? = null
        var failRemove: String? = null
        val writes = mutableListOf<String>()
        override fun read(key: String) = synchronized(values) {
            if (key == failRead) throw IOException("secure store unavailable")
            values[key]?.clone()
        }
        override fun write(key: String, value: ByteArray) = synchronized(values) {
            if (key == failWrite) throw IOException("secure store unavailable")
            writes += key
            values[key] = value.clone()
        }
        override fun remove(key: String) {
            synchronized(values) {
                if (key == failRemove) throw IOException("secure store unavailable")
                values.remove(key)
            }
        }
    }
    private class Server : HostedTransport {
        var accountId = UUID.randomUUID().toString()
        var operationId: String? = null
        var reportedOperationId: String? = null
        var vaultId: String? = null
        var access = "read_write"
        var loseComplete = false
        var expireGrant = false
        var beforeSession: (() -> Unit)? = null
        var beforeRenew: (() -> Unit)? = null
        var beforeAccount: (() -> Unit)? = null
        var renewCount = 0
        var renewResponse = 200
        var accountResponse = 200
        var sessionToken = "a"
        var beforeGrant: (() -> Unit)? = null
        var onAccount: (() -> Unit)? = null
        var grantCount = 0
        val completeBodies = mutableListOf<String>()
        val calls = mutableListOf<String>()
        override fun get(path: String, bearer: String?): HttpResult {
            calls += path
            return when (path) {
                "/hosted/v1/auth/config" -> HttpResult(200, """{"available_providers":["google"]}""")
                "/hosted/v1/account" -> {
                    assertNotNull(bearer)
                    val responseCode = accountResponse
                    beforeAccount?.invoke()
                    if (responseCode != 200) return HttpResult(responseCode, """{"error":"unauthorized"}""")
                    onAccount?.invoke()
                    HttpResult(200, JSONObject().put("account_id", accountId)
                        .put("classification", if (vaultId == null) "incomplete" else "existing")
                        .put("entitlement", "active").put("access", access)
                        .apply { (reportedOperationId ?: operationId)?.let { put("operation_id", it) }; vaultId?.let { put("vault_id", it) } }.toString())
                }
                else -> throw AssertionError("Unknown GET route $path")
            }
        }
        override fun post(path: String, body: String, bearer: String?): HttpResult {
            calls += path
            return when (path) {
                "/hosted/v1/auth/attempts" -> HttpResult(200, """{"attempt_id":"$accountId","nonce":"pst_${"b".repeat(64)}","expires_in_seconds":300}""")
                "/hosted/v1/auth/session" -> {
                    beforeSession?.invoke()
                    assertEquals(accountId, JSONObject(body).getString("attempt_id"))
                    HttpResult(200, """{"session_token":"pst_${sessionToken.repeat(64)}","expires_in_seconds":3600,"account_id":"$accountId"}""")
                }
                "/hosted/v1/auth/session/renew" -> {
                    assertEquals("{}", body)
                    assertNotNull(bearer)
                    renewCount++
                    beforeRenew?.invoke()
                    if (renewResponse != 200) HttpResult(renewResponse, """{"error":"unauthorized"}""")
                    else HttpResult(200, """{"account_id":"$accountId","expires_in_seconds":2592000}""")
                }
                "/hosted/v1/provisioning" -> {
                    beforeGrant?.invoke(); grantCount++
                    operationId = JSONObject(body).getString("operation_id")
                    HttpResult(200, """{"grant":"pgr_${if (grantCount == 1) "a".repeat(64) else "b".repeat(64)}","expires_in_seconds":600}""")
                }
                "/hosted/v1/provisioning/complete" -> {
                    completeBodies += body
                    if (expireGrant) { expireGrant = false; return HttpResult(401, """{"error":"unauthorized"}""") }
                    val request = JSONObject(body)
                    vaultId = request.getJSONObject("public_key_profile").getString("vault_id")
                    if (loseComplete) { loseComplete = false; throw IOException("response lost after commit") }
                    HttpResult(200, JSONObject().put("operation_id", operationId).put("vault_id", vaultId)
                        .put("device_id", request.getString("device_id")).put("already_provisioned", completeBodies.size > 1).toString())
                }
                else -> throw AssertionError("Unknown POST route $path")
            }
        }
        override fun delete(path: String, bearer: String): HttpResult {
            assertEquals("/hosted/v1/auth/session", path)
            return HttpResult(204, null)
        }
    }
    private class Fixture {
        val store = Store()
        val server = Server()
        var now = 0L
        var client = HostedAccountClient(store, server) { now }
        suspend fun login() = client.finishGoogleSignIn(server.accountId, "provider-token")
        fun restart() { client = HostedAccountClient(store, server) { now } }
        fun pendingKey(accountId: String = server.accountId) =
            VaultSecretStore(store).pendingAccount(HostedAccountClient.ORIGIN, accountId)
        fun pending(accountId: String = server.accountId) = store.values[pendingKey(accountId)]?.toString(Charsets.UTF_8)
        fun binding(vaultId: String, deviceId: String) = store.values[VaultSecretStore(store).account(vaultId, deviceId)]?.toString(Charsets.UTF_8)
    }
    private suspend inline fun <reified T : Throwable> rejects(crossinline block: suspend () -> Unit) {
        try { block() } catch (error: Throwable) { assertTrue("Expected ${T::class}, got ${error::class}", error is T); return }
        fail("Expected ${T::class}")
    }
    private val phrase = "velvet orchard lantern canyon glacier silver"

    @Test fun usesRealRoutesAndProvisionsSubscribedFirstVault() = runBlocking {
        val f = Fixture()
        assertEquals(setOf("google"), f.client.availableProviders())
        f.client.beginGoogleSignIn().use { assertEquals(f.server.accountId, it.attemptId()); assertTrue(it.nonce().startsWith("pst_")) }
        val account = f.login()
        assertEquals("incomplete", account.classification)
        assertNull(account.operationId)
        val prepared = f.client.prepareVault(phrase)
        val credential = CredentialParser.parse(f.client.completeVault(phrase))!!
        assertEquals(prepared.vaultId, credential.vaultId)
        assertEquals(prepared.deviceId, credential.deviceId)
        f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId)
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(phrase, f.binding(prepared.vaultId, prepared.deviceId))
        assertNull(f.pending())
    }

    @Test fun storeFailurePreventsFollowingRemoteWrite() = runBlocking {
        val f = Fixture(); f.login()
        f.store.failWrite = HostedAccountClient.CHECKPOINT_KEY
        rejects<IOException> { f.client.prepareVault(phrase) }
        assertEquals(0, f.server.grantCount)
        f.store.failWrite = null
        f.client.prepareVault(phrase)
        val original = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        f.store.failWrite = HostedAccountClient.CHECKPOINT_KEY
        rejects<IOException> { f.client.completeVault(phrase) }
        assertEquals(1, f.server.grantCount)
        assertTrue(f.server.completeBodies.isEmpty())
        assertArrayEquals(original, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun lostCompletionReplaysOriginalMaterialAndGrant() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase)
        f.server.loseComplete = true
        rejects<IOException> { f.client.completeVault(phrase) }
        val afterLostResponse = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        val credential = CredentialParser.parse(f.client.completeVault(phrase))!!
        assertEquals(1, f.server.grantCount)
        assertEquals(f.server.completeBodies[0], f.server.completeBodies[1])
        assertEquals(prepared.vaultId, credential.vaultId)
        assertArrayEquals(afterLostResponse, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun corruptAndOtherAccountCheckpointsAreNeverReplaced() = runBlocking {
        val f = Fixture(); f.login()
        f.store.write(HostedAccountClient.CHECKPOINT_KEY, "corrupt".toByteArray())
        rejects<HostedFailure> { f.client.prepareVault(phrase) }
        assertEquals("corrupt", String(f.store.read(HostedAccountClient.CHECKPOINT_KEY)!!))
        f.store.remove(HostedAccountClient.CHECKPOINT_KEY)
        f.client.prepareVault(phrase)
        val original = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        f.server.accountId = UUID.randomUUID().toString(); f.login()
        rejects<HostedFailure> { f.client.prepareVault(phrase) }
        assertArrayEquals(original, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(0, f.server.grantCount)
    }

    @Test fun expiredSessionMakesNoWriteAndKeepsPendingSetup() = runBlocking {
        val f = Fixture(); f.login(); f.client.prepareVault(phrase)
        val checkpoint = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        f.now = 3_600_001
        rejects<HostedFailure> { f.client.completeVault(phrase) }
        assertEquals(0, f.server.grantCount)
        assertArrayEquals(checkpoint, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun qualifyingInteractionRenewsAnExpiredLocalSession() = runBlocking {
        val f = Fixture(); f.login()
        f.now = 3_600_001

        f.client.renewAfterUserInteraction()

        assertEquals(1, f.server.renewCount)
        val stored = JSONObject(String(f.store.read(HostedAccountClient.SESSION_KEY)!!))
        assertEquals(3_600_001L + 2_592_000_000L, stored.getLong("expires_at"))
        f.client.renewAfterUserInteraction()
        assertEquals(1, f.server.renewCount)
    }

    @Test fun successfulRenewalCannotOverwriteANewerSameAccountLogin() = runBlocking {
        val f = Fixture(); f.login(); f.now = 3_600_001
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        f.server.beforeRenew = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        val renewal = async { rejects<kotlinx.coroutines.CancellationException> { f.client.renewAfterUserInteraction() } }
        withContext(Dispatchers.IO) { assertTrue(entered.await(5, TimeUnit.SECONDS)) }
        f.server.sessionToken = "b"
        HostedAccountClient(f.store, f.server) { f.now }.finishGoogleSignIn(f.server.accountId, "provider-token")
        release.countDown(); renewal.await()

        val stored = JSONObject(String(f.store.read(HostedAccountClient.SESSION_KEY)!!))
        assertEquals("pst_${"b".repeat(64)}", stored.getString("bearer"))
    }

    @Test fun successfulRenewalCannotRestoreSessionAfterOtherClientSignsOut() = runBlocking {
        val f = Fixture(); f.login(); f.client.prepareVault(phrase); f.now = 3_600_001
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        f.server.beforeRenew = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        val renewal = async { rejects<kotlinx.coroutines.CancellationException> { f.client.renewAfterUserInteraction() } }
        withContext(Dispatchers.IO) { assertTrue(entered.await(5, TimeUnit.SECONDS)) }
        HostedAccountClient(f.store, f.server) { f.now }.signOut()
        release.countDown(); renewal.await()

        assertNull(f.store.read(HostedAccountClient.SESSION_KEY))
        assertNotNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun staleAccount401CannotRemoveANewerSameAccountLogin() = runBlocking {
        val f = Fixture(); f.login()
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        f.server.accountResponse = 401
        f.server.beforeAccount = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        val account = async { rejects<HostedFailure> { f.client.account() } }
        withContext(Dispatchers.IO) { assertTrue(entered.await(5, TimeUnit.SECONDS)) }
        f.server.beforeAccount = null
        f.server.sessionToken = "b"
        f.server.accountResponse = 200
        HostedAccountClient(f.store, f.server) { f.now }.finishGoogleSignIn(f.server.accountId, "provider-token")
        release.countDown(); account.await()

        val stored = JSONObject(String(f.store.read(HostedAccountClient.SESSION_KEY)!!))
        assertEquals("pst_${"b".repeat(64)}", stored.getString("bearer"))
    }

    @Test fun lateLoginCannotUndoSignOut() = runBlocking {
        val f = Fixture(); val entered = CountDownLatch(1); val release = CountDownLatch(1)
        f.server.beforeSession = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        val login = async { rejects<kotlinx.coroutines.CancellationException> { f.login() } }
        withContext(Dispatchers.IO) { assertTrue(entered.await(5, TimeUnit.SECONDS)) }
        f.client.signOut(); release.countDown(); login.await()
        assertNull(f.store.read(HostedAccountClient.SESSION_KEY))
    }

    @Test fun canceledGrantCannotOverwriteCheckpointOrComplete() = runBlocking {
        val f = Fixture(); f.login(); f.client.prepareVault(phrase)
        val checkpoint = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        val entered = CompletableDeferred<Unit>(); val release = CountDownLatch(1)
        f.server.beforeGrant = { entered.complete(Unit); check(release.await(30, TimeUnit.SECONDS)) }
        val complete = async { rejects<kotlinx.coroutines.CancellationException> { f.client.completeVault(phrase) } }
        entered.await() // Real Argon2 verification precedes this boundary; synchronize on the request, not its speed.
        f.client.cancelPendingWork(); release.countDown(); complete.await()
        assertArrayEquals(checkpoint, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertTrue(f.server.completeBodies.isEmpty())
    }

    @Test fun expiredGrantRotatesOnlySamePendingOperation() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase)
        f.server.reportedOperationId = UUID.randomUUID().toString() // Account summary may list a different pending attempt.
        f.server.expireGrant = true
        val credential = CredentialParser.parse(f.client.completeVault(phrase))!!
        assertEquals(2, f.server.grantCount)
        assertEquals(prepared.vaultId, credential.vaultId)
        val first = JSONObject(f.server.completeBodies[0]); val second = JSONObject(f.server.completeBodies[1])
        assertNotEquals(first.remove("grant"), second.remove("grant"))
        assertEquals(first.toString(), second.toString())
    }

    @Test fun pendingSecretIsDurableBeforeTheCheckpointAndSurvivesRestartAndSignOut() = runBlocking {
        val f = Fixture(); f.login()
        f.store.failWrite = HostedAccountClient.CHECKPOINT_KEY
        rejects<IOException> { f.client.prepareVault(phrase) }
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(phrase, f.pending())
        assertEquals(0, f.server.grantCount)
        f.store.failWrite = null
        f.client.signOut(); f.restart(); f.login()
        assertEquals(phrase, f.client.candidatePendingSecret())
        rejects<HostedFailure> { f.client.prepareVault(generateHostedPassphrase()) }
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(phrase, f.pending())
        val prepared = f.client.prepareVault(phrase)
        CredentialParser.parse(f.client.completeVault(phrase))!!
        f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId)
        assertEquals(phrase, f.binding(prepared.vaultId, prepared.deviceId))
        assertNull(f.client.candidatePendingSecret())
    }

    @Test fun pendingSecretWriteFailureStopsBeforeTheCheckpoint() = runBlocking {
        val f = Fixture(); f.login()
        f.store.failWrite = f.pendingKey()
        rejects<HostedFailure> { f.client.prepareVault(phrase) }
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(0, f.server.grantCount)
    }

    @Test fun pendingSecretIsScopedToItsAccount() = runBlocking {
        val f = Fixture(); f.login()
        val first = f.server.accountId
        f.store.failWrite = HostedAccountClient.CHECKPOINT_KEY
        rejects<IOException> { f.client.prepareVault(phrase) }
        f.store.failWrite = null
        f.server.accountId = UUID.randomUUID().toString(); f.login()
        assertNull(f.client.candidatePendingSecret())
        val other = generateHostedPassphrase()
        f.client.prepareVault(other)
        assertEquals(other, f.pending())
        assertEquals(phrase, f.pending(first))
    }

    @Test fun checkpointWithoutPendingSecretPersistsItBeforeAnyPost() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase)
        f.store.values.remove(f.pendingKey())
        f.client.prepareVault(phrase)
        assertEquals(phrase, f.pending())
        f.store.values.remove(f.pendingKey())
        f.server.beforeGrant = { assertEquals(phrase, f.pending()) }
        CredentialParser.parse(f.client.completeVault(phrase))!!
        assertEquals(1, f.server.grantCount)
        f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId)
        assertEquals(phrase, f.binding(prepared.vaultId, prepared.deviceId))
    }

    @Test fun wrongPhraseNeverPersistsPendingOrPosts() = runBlocking {
        val f = Fixture(); f.login(); f.client.prepareVault(phrase)
        f.store.values.remove(f.pendingKey())
        val wrong = generateHostedPassphrase()
        rejects<HostedFailure> { f.client.prepareVault(wrong) }
        rejects<HostedFailure> { f.client.completeVault(wrong) }
        assertNull(f.pending())
        assertEquals(0, f.server.grantCount)
    }

    @Test fun unreadablePendingSecretBlocksTheGrant() = runBlocking {
        val f = Fixture(); f.login(); f.client.prepareVault(phrase)
        f.store.failRead = f.pendingKey()
        rejects<HostedFailure> { f.client.completeVault(phrase) }
        rejects<HostedFailure> { f.client.candidatePendingSecret() }
        assertEquals(0, f.server.grantCount)
        assertTrue(f.server.completeBodies.isEmpty())
    }

    @Test fun canceledPreparationWritesNoSecretOrCheckpoint() = runBlocking {
        val f = Fixture(); f.login()
        f.server.onAccount = { f.client.cancelPendingWork() }
        rejects<kotlinx.coroutines.CancellationException> { f.client.prepareVault(phrase) }
        assertNull(f.pending())
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun bindingVerifiesTheCheckpointIdentityBeforeMovingTheSecret() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase); f.client.completeVault(phrase)
        val checkpoint = f.store.read(HostedAccountClient.CHECKPOINT_KEY)
        rejects<HostedFailure> { f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, "other-device") }
        rejects<HostedFailure> { f.client.bindPendingSecretAndAcknowledge("other-vault", prepared.deviceId) }
        assertTrue(f.store.writes.none { it.startsWith("vault-secret:v2:") })
        assertEquals(phrase, f.pending())
        assertArrayEquals(checkpoint, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        f.server.accountId = UUID.randomUUID().toString(); f.login()
        rejects<HostedFailure> { f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId) }
        assertNull(f.binding(prepared.vaultId, prepared.deviceId))
        assertArrayEquals(checkpoint, f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun missingCheckpointBindsNothing() = runBlocking {
        val f = Fixture(); f.login()
        assertTrue(VaultSecretStore(f.store).savePending(HostedAccountClient.ORIGIN, f.server.accountId, phrase.toCharArray()))
        f.client.bindPendingSecretAndAcknowledge("vault", "device")
        assertNull(f.binding("vault", "device"))
        assertEquals(phrase, f.pending())
    }

    @Test fun existingDifferentBindingIsNeverOverwrittenAndKeepsTheCheckpoint() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase); f.client.completeVault(phrase)
        val key = VaultSecretStore(f.store).account(prepared.vaultId, prepared.deviceId)
        f.store.values[key] = "bound".toByteArray()
        rejects<HostedFailure> { f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId) }
        assertEquals("bound", f.binding(prepared.vaultId, prepared.deviceId))
        assertEquals(phrase, f.pending())
        assertNotNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
    }

    @Test fun failedPendingRemovalKeepsTheCheckpointAndRetryCompletes() = runBlocking {
        val f = Fixture(); f.login(); val prepared = f.client.prepareVault(phrase); f.client.completeVault(phrase)
        f.store.failRemove = f.pendingKey()
        rejects<HostedFailure> { f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId) }
        assertNotNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(phrase, f.binding(prepared.vaultId, prepared.deviceId))
        f.store.failRemove = HostedAccountClient.CHECKPOINT_KEY
        rejects<IOException> { f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId) }
        assertNull(f.pending())
        assertNotNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        f.store.failRemove = null
        f.client.bindPendingSecretAndAcknowledge(prepared.vaultId, prepared.deviceId)
        assertNull(f.store.read(HostedAccountClient.CHECKPOINT_KEY))
        assertEquals(phrase, f.binding(prepared.vaultId, prepared.deviceId))
    }
}
