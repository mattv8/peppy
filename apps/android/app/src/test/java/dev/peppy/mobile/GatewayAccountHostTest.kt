package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.attachmentRetentionResponseReadLimitBytes

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayAccountHostTest : GatewayTestBase() {
    @Test fun loadUsesGeneratedStatusParserAndClassifiesAvailabilityResponses() {
        val responses = ArrayDeque(
            listOf(
                LoopbackHttpServer.Response(200, statusBody()),
                LoopbackHttpServer.Response(501, """{"code":"attachment_retention_unsupported"}"""),
                LoopbackHttpServer.Response(403, """{"code":"operation_not_permitted"}"""),
                LoopbackHttpServer.Response(503, """{"code":"attachment_retention_unavailable"}"""),
                LoopbackHttpServer.Response(401),
            ),
        )
        LoopbackHttpServer { request ->
            assertEquals("GET", request.method)
            assertEquals("/v1/attachments/storage/status", request.target)
            assertEquals("Bearer $TEST_TOKEN", request.headers["authorization"])
            responses.removeFirst()
        }.use { server ->
            enrollLoopback(server.origin)
            val scope = checkNotNull(GatewayAccountHost.attachmentStorageScope(context))

            val available = GatewayAccountHost.loadAttachmentStorage(context, scope)
            assertTrue(available is AttachmentStorageLoad.Available)
            assertEquals(12345L, (available as AttachmentStorageLoad.Available).status.usedBytes)
            assertEquals(AttachmentStorageLoad.Unsupported, GatewayAccountHost.loadAttachmentStorage(context, scope))
            assertEquals(AttachmentStorageLoad.TemporarilyUnavailable, GatewayAccountHost.loadAttachmentStorage(context, scope))
            assertEquals(AttachmentStorageLoad.TemporarilyUnavailable, GatewayAccountHost.loadAttachmentStorage(context, scope))
            assertEquals(AttachmentStorageLoad.AuthorizationFailure, GatewayAccountHost.loadAttachmentStorage(context, scope))
        }
    }

    @Test fun oversizedStatusBodyIsTemporarilyUnavailable() {
        LoopbackHttpServer { LoopbackHttpServer.Response(200, "x".repeat(attachmentRetentionResponseReadLimitBytes().toInt() + 1)) }.use { server ->
            enrollLoopback(server.origin)
            assertEquals(
                AttachmentStorageLoad.TemporarilyUnavailable,
                GatewayAccountHost.loadAttachmentStorage(context, checkNotNull(GatewayAccountHost.attachmentStorageScope(context))),
            )
        }
    }

    @Test fun rotatedCredentialAbortsAcknowledgementAfterTheRequest() {
        var requests = 0
        LoopbackHttpServer {
            requests++
            val rotated = checkNotNull(CredentialParser.parse(credentialJson(serverOrigin, vaultId, deviceId, "ef".repeat(48)).toByteArray(), allowDebugLoopback = true))
            assertEquals(ImportResult.UPDATED, NativeGateway.persistVerified(context, rotated, vaultMaterial()))
            LoopbackHttpServer.Response(204)
        }.use { server ->
            serverOrigin = server.origin
            enrollLoopback(server.origin)
            val scope = checkNotNull(GatewayAccountHost.attachmentStorageScope(context))
            assertFalse(GatewayAccountHost.acknowledgeAttachmentStorageNotice(context, scope))
            assertEquals(1, requests)
        }
    }

    @Test fun closedSessionAbortsAcknowledgementAfterTheRequest() {
        LoopbackHttpServer {
            NativeGateway.closeForTest()
            LoopbackHttpServer.Response(204)
        }.use { server ->
            enrollLoopback(server.origin)
            val scope = checkNotNull(GatewayAccountHost.attachmentStorageScope(context))

            assertFalse(GatewayAccountHost.acknowledgeAttachmentStorageNotice(context, scope))
        }
    }

    private lateinit var serverOrigin: String

    private fun enrollLoopback(origin: String) {
        val credential = checkNotNull(CredentialParser.parse(credentialJson(origin, vaultId, deviceId).toByteArray(), allowDebugLoopback = true))
        assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credential, vaultMaterial()))
    }

    private fun statusBody() = """{"used_bytes":12345,"limit_bytes":23456,"base_bytes":10000,"pack_bytes":2345,"protected_bytes":8765,"removed_bytes":1234,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready","notice":{"state":"grace_period","policy_version":"hosted_attachment_retention_v1","presented_at":1700000000,"acknowledged_at":null,"activation_at":1702592000}}"""
}
