package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.Assert.assertNotNull
import uniffi.peppy_mobile_bindings.attachmentRetentionProtocolHeader

class OwnerPhoneApprovalFactoryTest {
    @Test fun selfHostedOriginNeverReadsOrForwardsTheHostedAccountSession() {
        var reads = 0
        val session = ownerPhoneAccountSession("https://community.example") {
            reads++
            "hosted-session"
        }
        assertNull(session)
        assertEquals(0, reads)
    }

    @Test fun hostedOriginForwardsTheAccountSession() {
        assertEquals("hosted-session", ownerPhoneAccountSession(HostedAccountClient.ORIGIN) { "hosted-session" })
    }

    @Test fun ownerEnrollmentMutationCarriesCanonicalRetentionHeader() {
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        LoopbackHttpServer { request -> requests += request; LoopbackHttpServer.Response(200, "{}") }.use { server ->
            val credential = GatewayAccountCredential(server.origin, "vault", "device", "token", 1L)
            val response = ownerPhoneRequest(
                credential, "POST", "/v1/enrollment/computers", "{}", null, attachmentRetentionProtocolHeader(),
            )
            assertNotNull(response)
            assertEquals("1", requests.single().headers["peppy-attachment-retention-protocol"])
        }
    }
}
