package dev.peppy.mobile.restrictedsetup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

class EphemeralAdbIdentityTest {
    @Test fun `creates a fresh RSA identity and drops references when destroyed`() {
        val first = EphemeralAdbIdentity.create()
        val second = EphemeralAdbIdentity.create()

        assertEquals("RSA", first.privateKey.algorithm)
        assertEquals("Peppy", EphemeralAdbIdentity.DEVICE_NAME)
        assertNotNull(first.certificate)
        assertNotEquals(first.certificate.serialNumber, second.certificate.serialNumber)
        first.destroy()
        runCatching { first.privateKey }.onSuccess { error("destroyed identity retained its key") }
    }
}
