package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

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
}
