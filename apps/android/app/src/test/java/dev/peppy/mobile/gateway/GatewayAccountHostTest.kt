package dev.peppy.mobile.gateway

import dev.peppy.mobile.LoopbackHttpServer
import dev.peppy.mobile.attachments.AttachmentStorageLoad
import dev.peppy.mobile.security.CredentialParser
import java.util.UUID
import org.junit.Assert.*
import org.junit.Assert.assertEquals
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

    @Test fun loadParsesNameKindAndPresenceForEachDeviceRow() {
        val otherId = UUID.randomUUID().toString()
        LoopbackHttpServer { request ->
            assertEquals("/v1/devices", request.target)
            LoopbackHttpServer.Response(
                200,
                """{"devices":[
                    {"device_id":"$deviceId","name":"  Work phone  ","kind":"android","role":"gateway","revoked":false,"key_epoch":3,"presence":{"state":"seen","last_seen_age_seconds":42}},
                    {"device_id":"$otherId","name":null,"kind":null,"role":"device","revoked":false,"key_epoch":1}
                ]}""",
            )
        }.use { server ->
            enrollLoopback(server.origin)
            val state = checkNotNull(GatewayAccountHost.load(context))
            assertEquals(
                GatewayAccountDevice(
                    deviceId, "gateway", false, 3,
                    name = "Work phone", kind = "android",
                    presence = DevicePresence(DevicePresenceState.SEEN, 42L),
                ),
                state.self,
            )
            assertEquals(2, state.devices.size)
            assertEquals(GatewayAccountDevice(otherId, "device", false, 1), state.devices[1])
        }
    }

    @Test fun loadMapsUnknownPresenceStateToUnknown() {
        LoopbackHttpServer {
            LoopbackHttpServer.Response(
                200,
                """{"devices":[{"device_id":"$deviceId","role":"gateway","revoked":false,"key_epoch":0,"presence":{"state":"sleeping"}}]}""",
            )
        }.use { server ->
            enrollLoopback(server.origin)
            val presence = checkNotNull(GatewayAccountHost.load(context)).self.presence
            assertEquals(DevicePresence(DevicePresenceState.UNKNOWN, null), presence)
        }
    }

    @Test fun loadTreatsNonNumericPresenceAgeAsNull() {
        LoopbackHttpServer {
            LoopbackHttpServer.Response(
                200,
                """{"devices":[{"device_id":"$deviceId","role":"gateway","revoked":false,"key_epoch":0,"presence":{"state":"seen","last_seen_age_seconds":"5"}}]}""",
            )
        }.use { server ->
            enrollLoopback(server.origin)
            val presence = checkNotNull(GatewayAccountHost.load(context)).self.presence
            assertEquals(DevicePresence(DevicePresenceState.SEEN, null), presence)
        }
    }

    @Test fun renameAcceptsExactly128BytesOfMultiByteName() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(204) }.use { server ->
            enrollLoopback(server.origin)

            assertEquals(RenameResult.Renamed, GatewayAccountHost.rename(context, deviceId, "é".repeat(64)))
            assertEquals(1, requests.size)
        }
    }

    @Test fun renameRejects130BytesOfMultiByteNameWithoutSendingRequest() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(204) }.use { server ->
            enrollLoopback(server.origin)

            assertEquals(RenameResult.InvalidName, GatewayAccountHost.rename(context, deviceId, "é".repeat(65)))
            assertEquals(0, requests.size)
        }
    }

    @Test fun renameSendsTrimmedNameAndMapsServerOutcomes() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        val responses = ArrayDeque(
            listOf(
                LoopbackHttpServer.Response(204),
                LoopbackHttpServer.Response(400, """{"code":"invalid_device_name"}"""),
                LoopbackHttpServer.Response(500),
            ),
        )
        LoopbackHttpServer { request ->
            requests += request
            responses.removeFirst()
        }.use { server ->
            enrollLoopback(server.origin)

            assertEquals(RenameResult.Renamed, GatewayAccountHost.rename(context, deviceId, "  Trimmed  "))
            assertEquals("PUT", requests.single().method)
            assertEquals("/v1/devices/$deviceId/name", requests.single().target)
            assertEquals("{\"name\":\"Trimmed\"}", requests.single().body)

            assertEquals(RenameResult.InvalidName, GatewayAccountHost.rename(context, deviceId, "Server says no"))
            assertEquals(RenameResult.Failed, GatewayAccountHost.rename(context, deviceId, "Broken"))
        }
    }

    @Test fun renameRejectsInvalidNamesWithoutSendingRequests() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(204) }.use { server ->
            enrollLoopback(server.origin)

            assertEquals(RenameResult.InvalidName, GatewayAccountHost.rename(context, deviceId, "   "))
            assertEquals(RenameResult.InvalidName, GatewayAccountHost.rename(context, deviceId, "x".repeat(129)))
            assertEquals(RenameResult.InvalidName, GatewayAccountHost.rename(context, deviceId, "Line\nbreak"))
            assertEquals(0, requests.size)
        }
    }

    @Test fun renameFailsForNonUuidTargetWithoutSendingRequests() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(204) }.use { server ->
            enrollLoopback(server.origin)

            assertEquals(RenameResult.Failed, GatewayAccountHost.rename(context, "not-a-uuid", "Phone"))
            assertEquals(0, requests.size)
        }
    }

    private fun enrollLoopback(origin: String) {
        val credential = checkNotNull(CredentialParser.parse(credentialJson(origin, vaultId, deviceId).toByteArray(), allowDebugLoopback = true))
        assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credential, vaultMaterial()))
    }

    private fun statusBody() = """{"used_bytes":12345,"limit_bytes":23456,"base_bytes":10000,"pack_bytes":2345,"protected_bytes":8765,"removed_bytes":1234,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready"}"""
}
