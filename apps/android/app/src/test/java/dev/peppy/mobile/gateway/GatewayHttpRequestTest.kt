package dev.peppy.mobile.gateway

import dev.peppy.mobile.LoopbackHttpServer
import java.io.IOException
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeAttachmentRetentionProtocolHeader
import uniffi.peppy_mobile_bindings.NativeAttachmentRetentionRequest


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayHttpRequestTest {
    @Test fun beforeRequestDenialBlocksJsonMethodsWithoutCallingResponseObserver() {
        val requests = AtomicInteger()
        var observed = 0
        val denial = IOException("retired scope")
        LoopbackHttpServer { requests.incrementAndGet(); LoopbackHttpServer.Response(200, "{}") }.use { server ->
            // Preserve the existing trailing-lambda observer constructor contract.
            val http = GatewayHttp(server.origin, "token") { _ -> observed++ }
            http.setBeforeRequest { throw denial }
            val operations: List<() -> Unit> = listOf(
                { http.get("/v1/events") },
                { http.postJson("/v1/events", "{}") },
                { http.deleteJson("/v1/events/test", "{}") },
            )
            operations.forEach { operation -> assertSame(denial, assertThrows(IOException::class.java) { operation() }) }
        }
        assertEquals(0, requests.get())
        assertEquals(0, observed)
    }

    @Test fun beforeRequestDenialBlocksAllGeneratedRetentionMethods() {
        val requests = AtomicInteger()
        val denial = IllegalStateException("retired scope")
        LoopbackHttpServer { requests.incrementAndGet(); LoopbackHttpServer.Response(200, "{}") }.use { server ->
            val http = GatewayHttp(server.origin, "token")
            http.setBeforeRequest { throw denial }
            for (method in listOf("GET", "POST", "PUT", "DELETE")) {
                val request = NativeAttachmentRetentionRequest(method, "/v1/attachments/storage", if (method == "GET") null else "{}", null, 200u)
                assertSame(denial, assertThrows(IllegalStateException::class.java) { http.retentionRequest(request, 128u) })
            }
        }
        assertEquals(0, requests.get())
    }

    @Test fun beforeRequestIOExceptionCannotStartAnUploadThroughResponseRecovery() {
        val requests = AtomicInteger()
        val denial = IOException("retired scope")
        val source = Files.createTempFile("fenced-cipher", ".bin").toFile().apply { writeText("cipher") }
        try {
            LoopbackHttpServer { requests.incrementAndGet(); LoopbackHttpServer.Response(413, """{"code":"vault_quota_exceeded"}""") }.use { server ->
                val http = GatewayHttp(server.origin, "token")
                http.setBeforeRequest { throw denial }
                assertSame(denial, assertThrows(IOException::class.java) { http.putFile("/v1/attachments/test/upload", source, 6) })
            }
            assertEquals(0, requests.get())
        } finally { source.delete() }
    }

    @Test fun beforeRequestIOExceptionCannotStartADownloadThroughResponseRecovery() {
        val requests = AtomicInteger()
        val denial = IOException("retired scope")
        val destination = Files.createTempFile("fenced-download", ".part").toFile()
        try {
            LoopbackHttpServer { requests.incrementAndGet(); LoopbackHttpServer.Response(200, "cipher") }.use { server ->
                val http = GatewayHttp(server.origin, "token")
                http.setBeforeRequest { throw denial }
                assertSame(denial, assertThrows(IOException::class.java) { http.getToFile("/v1/attachments/test", destination, 6) })
            }
            assertEquals(0, requests.get())
            assertFalse(destination.exists())
        } finally { destination.delete() }
    }

    @Test fun jsonRequestsSendExpectedMethodsPathsHeadersBodiesAndStatuses() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request ->
            requests += request
            when (request.target) {
                "/v1/events?after=7&limit=50" -> LoopbackHttpServer.Response(200, "[]")
                "/v1/messages" -> LoopbackHttpServer.Response(201, "{\"id\":\"message-1\"}")
                else -> LoopbackHttpServer.Response(403, "{\"code\":\"forbidden\"}")
            }
        }.use { server ->
            val http = GatewayHttp(server.origin, "test-token")

            val get = http.get("/v1/events?after=7&limit=50")
            val post = http.postJson("/v1/messages", "{\"body\":\"hello\"}")
            val delete = http.deleteJson("/v1/messages/message-1", "{\"reason\":\"withdrawn\"}")

            assertTrue(get.ok)
            assertEquals("[]", get.body)
            assertEquals(201, post.code)
            assertEquals("{\"id\":\"message-1\"}", post.body)
            assertEquals(403, delete.code)
            assertEquals("{\"code\":\"forbidden\"}", delete.body)
            assertEquals("forbidden", delete.errorCode)

            assertEquals(
                listOf("GET", "POST", "DELETE"),
                requests.map { it.method },
            )
            assertEquals(
                listOf("/v1/events?after=7&limit=50", "/v1/messages", "/v1/messages/message-1"),
                requests.map { it.target },
            )
            assertEquals(
                listOf("", "{\"body\":\"hello\"}", "{\"reason\":\"withdrawn\"}"),
                requests.map { it.body },
            )
            requests.forEach { request ->
                assertEquals("Bearer test-token", request.headers["authorization"])
                assertEquals("application/json", request.headers["accept"])
            }
            assertEquals("application/json", requests[1].headers["content-type"])
            assertEquals("application/json", requests[2].headers["content-type"])
            assertNull(requests[0].headers["content-type"])
        }
    }

    @Test fun jsonPathsRejectInvalidTargetsBeforeSendingRequests() {
        var requests = 0
        LoopbackHttpServer { requests++; LoopbackHttpServer.Response(200, "[]") }.use { server ->
            val http = GatewayHttp(server.origin, "test-token")

            assertThrows(IllegalArgumentException::class.java) { http.get("/v2/events") }
            assertThrows(IllegalArgumentException::class.java) { http.get("/v1/events#fragment") }

            assertEquals(0, requests)
        }
    }

    @Test fun retentionRequestUsesGeneratedMethodBodyHeaderAndBoundedResponse() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request ->
            requests += request
            LoopbackHttpServer.Response(200, "{\"use_id\":\"use\"}")
        }.use { server ->
            val request = NativeAttachmentRetentionRequest(
                "PUT",
                "/v1/attachments/uses/use",
                "{\"kind\":\"message_media\"}",
                NativeAttachmentRetentionProtocolHeader("Peppy-Attachment-Retention", "1"),
                200u,
            )

            val response = GatewayHttp(server.origin, "test-token").retentionRequest(request, 128u)

            assertEquals(200, response.code)
            assertEquals("{\"use_id\":\"use\"}", response.body)
            assertEquals("PUT", requests.single().method)
            assertEquals("1", requests.single().headers["peppy-attachment-retention"])
            assertEquals("{\"kind\":\"message_media\"}", requests.single().body)
        }
    }


    @Test fun retentionRequestSupportsGeneratedGetAndCanonicalInitializedHeader() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request ->
            requests += request
            LoopbackHttpServer.Response(200, "{}")
        }.use { server ->
            val http = GatewayHttp(server.origin, "test-token")
            http.initializeRetentionProtocol(true)
            val request = NativeAttachmentRetentionRequest("GET", "/v1/attachments/storage", null, null, 200u)

            assertEquals(200, http.retentionRequest(request, 128u).code)
            http.postJson("/v1/events", "{}")
            assertNull(requests[0].headers["peppy-attachment-retention-protocol"])
            assertEquals("1", requests[1].headers["peppy-attachment-retention-protocol"])
        }
    }

    @Test fun generatedRequestWithoutHeaderDoesNotInheritNegotiatedMutationHeader() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(204) }.use { server ->
            val http = GatewayHttp(server.origin, "test-token")
            http.initializeRetentionProtocol(true)
            val request = NativeAttachmentRetentionRequest("PUT", "/v1/devices/self/retention-capability", "{}", null, 204u)

            assertEquals(204, http.retentionRequest(request, 128u).code)
            assertNull(requests.single().headers["peppy-attachment-retention-protocol"])
        }
    }

    @Test fun retentionRequestRejectsInvalidUtf8() {
        LoopbackHttpServer { LoopbackHttpServer.Response(200, rawBody = byteArrayOf(0xC3.toByte(), 0x28)) }.use { server ->
            val request = NativeAttachmentRetentionRequest("GET", "/v1/attachments/storage", null, null, 200u)
            assertThrows(GatewayTransportException::class.java) {
                GatewayHttp(server.origin, "test-token").retentionRequest(request, 128u)
            }
        }
    }

    @Test fun jsonRequestsDoNotFollowRedirectsToAnotherOrigin() {
        var redirectedRequests = 0
        LoopbackHttpServer { redirectedRequests++; LoopbackHttpServer.Response(200, "[]") }.use { redirected ->
            LoopbackHttpServer {
                LoopbackHttpServer.Response(302, headers = mapOf("Location" to "${redirected.origin}/v1/events"))
            }.use { server ->
                val response = GatewayHttp(server.origin, "test-token").get("/v1/events")

                assertEquals(302, response.code)
                assertEquals(0, redirectedRequests)
            }
        }
    }
}
