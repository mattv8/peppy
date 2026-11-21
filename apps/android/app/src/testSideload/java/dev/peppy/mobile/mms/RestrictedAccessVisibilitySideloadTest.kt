package dev.peppy.mobile.mms

import org.junit.Assert.assertTrue
import org.junit.Test

class RestrictedAccessVisibilitySideloadTest {
    @Test
    fun `restricted access is visible on sideload`() {
        assertTrue(restrictedMessageAccessVisible())
    }
}
