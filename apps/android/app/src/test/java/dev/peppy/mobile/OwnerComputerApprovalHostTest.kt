package dev.peppy.mobile

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NoHandle

class OwnerComputerApprovalHostTest {
    private class Store : HostedSecureStore {
        var failRead = false
        private val values = mutableMapOf<String, ByteArray>()
        override fun read(key: String): ByteArray? { if (failRead) throw IllegalStateException(); return values[key]?.clone() }
        override fun write(key: String, value: ByteArray) { values[key] = value.clone() }
        override fun remove(key: String) = Unit
    }
    private class Client : OwnerComputerClient {
        var request = OwnerComputerRequest("https://server.example", "invite", "digest", "request")
        var view = OwnerComputerView(request.origin, request.invitationId, request.promptDigest, true, "123 456")
        var approvalBodies = mutableListOf<String>()
        var cancelled = 0
        override fun createRequest(qrJson: String) = request
        override fun consumeCreated(invitationId: String, promptDigest: String, createdJson: String) = Unit
        override fun view() = view
        override fun approve(invitationId: String, promptDigest: String, vaultKeyPhrase: CharArray, nativeClient: NativeClient): String {
            approvalBodies += "approval"; return "approval"
        }
        override fun cancel() { cancelled++ }
        override fun close() = Unit
    }
    private class Transport : OwnerPhoneTransport {
        val posts = mutableListOf<String>()
        val calls = mutableListOf<String>()
        var response = EnrollmentHttpResponse(200, "created")
        override fun request(method: String, path: String, body: String?, accountSession: String?): EnrollmentHttpResponse? {
            calls += "$method $path"
            if (method == "POST") posts += "$path:$body"
            return response
        }
    }
    private fun host(client: Client = Client(), store: Store = Store(), transport: Transport = Transport()) =
        Triple(OwnerComputerApprovalHost(client, transport, VaultSecretStore(store), "vault", "device", { NativeClient(NoHandle) }, "https://server.example", false, null), client, transport)

    @Test fun `cross origin code is rejected before transport`() = runBlocking {
        val (host, client, transport) = host().also { it.second.request = it.second.request.copy(origin = "https://other.example") }
        assertEquals(OwnerComputerApprovalHost.Result.Failed("invalid-qr"), host.scan("qr"))
        assertTrue(transport.posts.isEmpty())
        assertEquals(1, client.cancelled)
    }

    @Test fun `secure store read failure fails closed`() = runBlocking {
        val store = Store().apply { failRead = true }
        val (host, client, transport) = host(store = store)
        host.scan("qr")
        assertEquals(OwnerComputerApprovalHost.Result.Failed("secure-store-unavailable"), host.approve(true, SecretPrompt { "phrase".toCharArray() }))
        assertTrue(client.approvalBodies.isEmpty())
        assertEquals(1, transport.posts.size)
    }

    @Test fun `prompt replacement race is cancelled`() = runBlocking {
        val (host, client, transport) = host()
        host.scan("qr")
        assertEquals(OwnerComputerApprovalHost.Result.Cancelled, host.approve(true, SecretPrompt {
            client.view = client.view.copy(promptDigest = "replacement")
            "phrase".toCharArray()
        }))
        assertEquals(1, transport.posts.size)
    }

    @Test fun `codes must match before approval`() = runBlocking {
        val (host, client, _) = host()
        host.scan("qr")
        assertEquals(OwnerComputerApprovalHost.Result.Failed("codes-do-not-match"), host.approve(false, SecretPrompt { "phrase".toCharArray() }))
        assertTrue(client.approvalBodies.isEmpty())
    }

    @Test fun `stored phrase creates an explicit terminal approval using opaque bodies`() = runBlocking {
        val store = Store()
        VaultSecretStore(store).saveIfMissing("vault", "device", "phrase".toCharArray())
        val (host, client, transport) = host(store = store)
        assertTrue(host.scan("qr") is OwnerComputerApprovalHost.Result.View)
        assertTrue(host.approve(true, SecretPrompt { throw AssertionError("must not prompt") }) is OwnerComputerApprovalHost.Result.Approved)
        assertEquals(listOf("/v1/enrollment/computers:request", "/v1/enrollment/computers/invite/approve:approval"), transport.posts)
        assertEquals(1, client.approvalBodies.size)
    }

    @Test fun `approved request does not retry approval or send cancellation`() = runBlocking {
        val store = Store()
        VaultSecretStore(store).saveIfMissing("vault", "device", "phrase".toCharArray())
        val (host, client, transport) = host(store = store)
        host.scan("qr")
        assertTrue(host.approve(true, SecretPrompt { throw AssertionError("must not prompt") }) is OwnerComputerApprovalHost.Result.Approved)
        assertTrue(host.approve(true, SecretPrompt { throw AssertionError("must not prompt") }) is OwnerComputerApprovalHost.Result.Approved)
        assertTrue(host.cancel() is OwnerComputerApprovalHost.Result.Approved)
        assertEquals(1, client.approvalBodies.size)
        assertEquals(0, client.cancelled)
        assertTrue(transport.calls.none { it.startsWith("DELETE") })
    }
}
