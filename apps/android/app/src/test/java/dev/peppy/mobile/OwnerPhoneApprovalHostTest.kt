package dev.peppy.mobile

import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.Executors

class OwnerPhoneApprovalHostTest {
    private class MemoryStore : HostedSecureStore {
        val values = mutableMapOf<String, ByteArray>()
        var failRead = false
        var failWrite = false
        override fun read(key: String): ByteArray? { if (failRead) throw IllegalStateException(); return values[key]?.clone() }
        override fun write(key: String, value: ByteArray) { if (failWrite) throw IllegalStateException(); values[key] = value.clone() }
        override fun remove(key: String) = Unit
    }

    private class Client : OwnerPhoneClient {
        var current = OwnerPhoneStatus(OwnerPhoneStage.VERIFY, 30, "123456", "claim", "invite")
        var matches = true
        var approvals = 0
        var method = OwnerPhoneMethod.TRANSFER
        var approvedStatus: OwnerPhoneStatus? = null
        override fun createInvitationRequest(method: OwnerPhoneMethod) = "create"
        override fun consumeCreated(createdJson: String) = Unit
        override fun qrPayload() = "qr"
        override fun status(statusJson: String) = current
        override fun invitationId() = current.invitationId
        override fun method() = method
        override fun secretMatches(secret: CharArray) = matches
        override fun approveRequest(statusJson: String, secret: CharArray): String {
            approvals++
            current = approvedStatus ?: current.copy(stage = OwnerPhoneStage.CONNECTED)
            return "approved"
        }
        override fun close() = Unit
    }

    private class Transport : OwnerPhoneTransport {
        val calls = mutableListOf<String>()
        var response = EnrollmentHttpResponse(200, "status")
        var approveResponse: EnrollmentHttpResponse? = null
        var approveOffline = false
        override fun request(method: String, path: String, body: String?, accountSession: String?): EnrollmentHttpResponse? {
            calls += "$method $path ${accountSession ?: "none"}"
            if (method == "POST" && path.endsWith("/approve")) return if (approveOffline) null else approveResponse ?: response
            return response
        }
    }

    private fun fixture(hosted: Boolean = false, accountSession: String? = null): Triple<OwnerPhoneApprovalHost, Client, Transport> {
        val client = Client()
        val transport = Transport()
        return Triple(
            OwnerPhoneApprovalHost(client, transport, VaultSecretStore(MemoryStore()), "vault", "device", { _, _, _, _ -> false }, hosted, accountSession),
            client,
            transport,
        )
    }

    private fun approvePosts(transport: Transport) = transport.calls.count { it.startsWith("POST /v1/enrollment/invitations/invite/approve") }

    @Test fun `stored matching secret approves without prompting`() = runBlocking {
        val secure = MemoryStore()
        val client = Client()
        val transport = Transport()
        val secrets = VaultSecretStore(secure)
        secrets.saveIfMissing("vault", "device", "secret".toCharArray())
        val host = OwnerPhoneApprovalHost(client, transport, secrets, "vault", "device", { _, _, _, _ -> false })
        val result = host.approve(SecretPrompt { throw AssertionError("must not prompt") })
        assertTrue(result is OwnerPhoneApprovalHost.Result.View)
        assertEquals(1, client.approvals)
    }

    @Test fun `stored secret approval never overwrites the saved secret`() = runBlocking {
        val secure = MemoryStore()
        val secrets = VaultSecretStore(secure)
        secrets.saveIfMissing("vault", "device", "saved".toCharArray())
        val host = OwnerPhoneApprovalHost(Client(), Transport(), secrets, "vault", "device", { _, _, _, _ -> false })
        host.approve(SecretPrompt { throw AssertionError("must not prompt") })
        assertEquals("saved", String((secrets.read("vault", "device") as StoredSecret.Present).value))
    }

    @Test fun `wrong prompted secret is rejected without an approval post or save`() = runBlocking {
        val secure = MemoryStore()
        val secrets = VaultSecretStore(secure)
        val (_, client, transport) = fixture()
        client.matches = false
        val host = OwnerPhoneApprovalHost(client, transport, secrets, "vault", "device", { _, _, _, _ -> false })
        assertEquals(OwnerPhoneApprovalHost.Result.Failed("wrong-passphrase"), host.approve(SecretPrompt { "wrong".toCharArray() }))
        assertEquals(0, client.approvals)
        assertEquals(0, approvePosts(transport))
        assertEquals(StoredSecret.Missing, secrets.read("vault", "device"))
    }

    @Test fun `stored secret mismatch fails closed without prompting`() = runBlocking {
        val secure = MemoryStore()
        val client = Client().apply { matches = false }
        VaultSecretStore(secure).saveIfMissing("vault", "device", "old".toCharArray())
        val host = OwnerPhoneApprovalHost(client, Transport(), VaultSecretStore(secure), "vault", "device", { _, _, _, _ -> false })
        assertEquals(OwnerPhoneApprovalHost.Result.Failed("vault-secret-mismatch"), host.approve(SecretPrompt { throw AssertionError("must not prompt") }))
        assertEquals(0, client.approvals)
    }

    @Test fun `storage read failure is unavailable and never prompts`() = runBlocking {
        val secure = MemoryStore().apply { failRead = true }
        val (_, client, transport) = fixture()
        val host = OwnerPhoneApprovalHost(client, transport, VaultSecretStore(secure), "vault", "device", { _, _, _, _ -> false })
        var prompts = 0
        assertEquals(
            OwnerPhoneApprovalHost.Result.Failed("secure-store-unavailable"),
            host.approve(SecretPrompt { prompts += 1; "secret".toCharArray() }),
        )
        assertEquals(0, prompts)
        assertEquals(0, approvePosts(transport))
    }

    @Test fun `prompt cancellation saves nothing and posts nothing`() = runBlocking {
        val secure = MemoryStore()
        val secrets = VaultSecretStore(secure)
        val (_, client, transport) = fixture()
        val host = OwnerPhoneApprovalHost(client, transport, secrets, "vault", "device", { _, _, _, _ -> false })
        assertEquals(OwnerPhoneApprovalHost.Result.Cancelled, host.approve(SecretPrompt { null }))
        assertEquals(StoredSecret.Missing, secrets.read("vault", "device"))
        assertEquals(0, approvePosts(transport))
    }

    @Test fun `claim changed during the prompt reaches restartable expired state without an approval post`() = runBlocking {
        val secure = MemoryStore()
        val secrets = VaultSecretStore(secure)
        val (_, client, transport) = fixture()
        val host = OwnerPhoneApprovalHost(client, transport, secrets, "vault", "device", { _, _, _, _ -> false })
        val result = host.approve(SecretPrompt {
            client.current = client.current.copy(claimDigest = "changed")
            "secret".toCharArray()
        })
        assertEquals(OwnerPhoneApprovalHost.Result.Failed("expired"), result)
        assertEquals(0, client.approvals)
        assertEquals(0, approvePosts(transport))
        assertEquals(StoredSecret.Missing, secrets.read("vault", "device"))
    }

    @Test fun `null digest terminal statuses do not discard a verified claim`() = runBlocking {
        val (host, client, transport) = fixture()
        client.current = client.current.copy(stage = OwnerPhoneStage.WAITING, claimDigest = null)
        assertTrue(host.poll() is OwnerPhoneApprovalHost.Result.View)

        client.current = client.current.copy(stage = OwnerPhoneStage.VERIFY, claimDigest = "claim-a")
        assertTrue(host.poll() is OwnerPhoneApprovalHost.Result.View)
        client.approvedStatus = client.current.copy(stage = OwnerPhoneStage.APPROVED)

        val result = host.approve(SecretPrompt { "secret".toCharArray() })
        assertEquals(OwnerPhoneStage.APPROVED, (result as OwnerPhoneApprovalHost.Result.View).status.stage)
        client.current = client.current.copy(stage = OwnerPhoneStage.CONNECTED, claimDigest = null)
        assertEquals(OwnerPhoneStage.CONNECTED, (host.poll() as OwnerPhoneApprovalHost.Result.View).status.stage)
        assertEquals(1, approvePosts(transport))
    }

    @Test fun `a changed displayed claim never posts for stored shared or prompted approval and restart remains usable`() = runBlocking {
        listOf("stored", "shared", "prompted").forEach { path ->
            val secure = MemoryStore()
            val secrets = VaultSecretStore(secure)
            val (host, client, transport) = fixture()
            if (path == "stored") secrets.saveIfMissing("vault", "device", "secret".toCharArray())
            if (path == "shared") client.method = OwnerPhoneMethod.SHARED_SECRET
            client.current = client.current.copy(claimDigest = "claim-a")
            assertTrue(host.poll() is OwnerPhoneApprovalHost.Result.View)
            client.current = client.current.copy(claimDigest = "claim-b")

            val result = host.approve(SecretPrompt { "secret".toCharArray() })

            assertEquals(OwnerPhoneApprovalHost.Result.Failed("expired"), result)
            assertEquals(path, 0, approvePosts(transport))
            transport.calls.clear()
            assertTrue(host.restart(OwnerPhoneMethod.TRANSFER) is OwnerPhoneApprovalHost.Result.View)
            assertTrue(path, transport.calls.any { it == "POST /v1/enrollment/invitations none" })
        }
    }

    @Test fun `failing save still approves a correct prompted secret`() = runBlocking {
        val secure = MemoryStore().apply { failWrite = true }
        val (_, client, transport) = fixture()
        val host = OwnerPhoneApprovalHost(client, transport, VaultSecretStore(secure), "vault", "device", { _, _, _, _ -> false })
        val result = host.approve(SecretPrompt { "secret".toCharArray() })
        assertTrue(result is OwnerPhoneApprovalHost.Result.View)
        assertEquals(1, client.approvals)
    }

    @Test fun `shared secret method approves without reading storage or prompting`() = runBlocking {
        val secure = MemoryStore().apply { failRead = true }
        val (_, client, transport) = fixture()
        client.method = OwnerPhoneMethod.SHARED_SECRET
        val host = OwnerPhoneApprovalHost(client, transport, VaultSecretStore(secure), "vault", "device", { _, _, _, _ -> false })
        assertTrue(host.approve(SecretPrompt { throw AssertionError("must not prompt") }) is OwnerPhoneApprovalHost.Result.View)
        assertEquals(1, client.approvals)
    }

    @Test fun `approve HTTP failures map to typed codes`() = runBlocking {
        val cases = listOf(
            null to "offline",
            EnrollmentHttpResponse(404, null) to "expired",
            EnrollmentHttpResponse(410, null) to "expired",
            EnrollmentHttpResponse(409, null) to "enrollment-state",
            EnrollmentHttpResponse(500, null) to "server-rejected",
        )
        for ((response, code) in cases) {
            val secure = MemoryStore()
            val secrets = VaultSecretStore(secure)
            secrets.saveIfMissing("vault", "device", "secret".toCharArray())
            val client = Client()
            val transport = Transport().apply { approveResponse = response; approveOffline = response == null }
            val host = OwnerPhoneApprovalHost(client, transport, secrets, "vault", "device", { _, _, _, _ -> false })
            assertEquals(OwnerPhoneApprovalHost.Result.Failed(code), host.approve(SecretPrompt { null }))
        }
    }

    @Test fun `approval without a response body stays approved`() = runBlocking {
        val (host, client, transport) = fixture()
        transport.approveResponse = EnrollmentHttpResponse(200, null)

        val result = host.approve(SecretPrompt { "secret".toCharArray() })

        assertEquals(OwnerPhoneStage.APPROVED, (result as OwnerPhoneApprovalHost.Result.View).status.stage)
        assertEquals(1, client.approvals)
    }

    @Test fun `approved invitation remains approved through expired poll then connects`() = runBlocking {
        val (host, client, transport) = fixture()

        host.approve(SecretPrompt { "secret".toCharArray() })
        transport.calls.clear()
        client.current = client.current.copy(stage = OwnerPhoneStage.EXPIRED)
        val expired = host.poll() as OwnerPhoneApprovalHost.Result.View
        client.current = client.current.copy(stage = OwnerPhoneStage.VERIFY)
        val staleVerify = host.poll() as OwnerPhoneApprovalHost.Result.View
        client.current = client.current.copy(stage = OwnerPhoneStage.CONNECTED)
        val connected = host.poll() as OwnerPhoneApprovalHost.Result.View

        assertEquals(OwnerPhoneStage.APPROVED, expired.status.stage)
        assertEquals(OwnerPhoneStage.APPROVED, staleVerify.status.stage)
        assertEquals(OwnerPhoneStage.CONNECTED, connected.status.stage)
        assertTrue(transport.calls.none { it.startsWith("POST /v1/enrollment/invitations") })
    }

    @Test fun `hosted requests require an account session before transport`() = runBlocking {
        val (host, _, transport) = fixture(hosted = true)
        assertEquals(OwnerPhoneApprovalHost.Result.Failed("account-required"), host.poll())
        assertTrue(transport.calls.isEmpty())
    }

    @Test fun `cancel treats an already gone waiting invitation as success`() = runBlocking {
        val (host, client, transport) = fixture()
        client.current = client.current.copy(stage = OwnerPhoneStage.WAITING)
        host.poll()
        transport.response = EnrollmentHttpResponse(404, null)
        assertEquals(OwnerPhoneApprovalHost.Result.Cancelled, host.cancel())
        assertTrue(transport.calls.last().startsWith("DELETE /v1/enrollment/invitations/invite"))
    }

    @Test fun `cancel never deletes a verify invitation`() = runBlocking {
        val (host, client, transport) = fixture()
        host.poll()
        assertEquals(OwnerPhoneStage.VERIFY, client.current.stage)
        assertEquals(OwnerPhoneApprovalHost.Result.Cancelled, host.cancel())
        assertTrue(transport.calls.none { it.startsWith("DELETE") })
    }

    @Test fun `rotation deletes the waiting invitation before creating a new one`() = runBlocking {
        val client = Client().apply { current = current.copy(stage = OwnerPhoneStage.WAITING) }
        val transport = Transport()
        val host = OwnerPhoneApprovalHost(client, transport, VaultSecretStore(MemoryStore()), "vault", "device", { _, _, _, _ -> true })
        host.rotateIfNeeded(OwnerPhoneMethod.SHARED_SECRET)
        assertTrue(transport.calls[0].startsWith("GET /v1/enrollment/invitations/invite"))
        assertTrue(transport.calls[1].startsWith("DELETE /v1/enrollment/invitations/invite"))
        assertEquals("POST /v1/enrollment/invitations none", transport.calls[2])
    }

    @Test fun `restart cancels the waiting invitation before creating another`() = runBlocking {
        val (host, client, transport) = fixture()
        client.current = client.current.copy(stage = OwnerPhoneStage.WAITING)
        host.poll()
        transport.calls.clear()
        host.restart(OwnerPhoneMethod.TRANSFER)
        assertTrue(transport.calls[0].startsWith("DELETE /v1/enrollment/invitations/invite"))
        assertEquals("POST /v1/enrollment/invitations none", transport.calls[1])
    }

    @Test fun `host work runs on the injected io dispatcher, not the caller thread`() {
        val executor = Executors.newSingleThreadExecutor { Thread(it, "owner-io-test") }
        val io = executor.asCoroutineDispatcher()
        try {
            val threads = mutableListOf<String>()
            val transport = OwnerPhoneTransport { _, _, _, _ ->
                threads += Thread.currentThread().name
                EnrollmentHttpResponse(200, "status")
            }
            val host = OwnerPhoneApprovalHost(
                Client(), transport, VaultSecretStore(MemoryStore()), "vault", "device", { _, _, _, _ -> false },
                ioDispatcher = io,
            )
            runBlocking { host.poll() }
            // kotlinx debug mode appends " @coroutine#n" to the thread name; compare the base name.
            assertEquals(listOf("owner-io-test"), threads.map { it.substringBefore(" @") })
        } finally {
            executor.shutdown()
        }
    }
}
