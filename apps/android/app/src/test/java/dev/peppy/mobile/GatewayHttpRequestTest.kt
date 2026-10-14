package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayHttpRequestTest {
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
