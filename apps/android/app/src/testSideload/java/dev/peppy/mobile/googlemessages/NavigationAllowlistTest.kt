package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NavigationAllowlistTest {
    @Test fun `permits only sign in and config routes`() {
        assertTrue(NavigationAllowlist.permits("https://accounts.google.com/AccountChooser"))
        assertTrue(NavigationAllowlist.permits("https://accounts.youtube.com/SetSID"))
        assertTrue(NavigationAllowlist.permits("https://www.google.com/accounts/CheckCookie"))
        assertTrue(NavigationAllowlist.permits("https://messages.google.com/accounts/SetOSID?continue=x"))
        assertTrue(NavigationAllowlist.permits("https://messages.google.com/web/config?pli=1"))
        assertFalse(NavigationAllowlist.permits("https://messages.google.com/web/conversations"))
        assertFalse(NavigationAllowlist.permits("https://example.com/"))
    }

    @Test fun `recognizes config without retaining its query`() {
        assertTrue(NavigationAllowlist.isConfig("https://messages.google.com/web/config?pli=1"))
        assertFalse(NavigationAllowlist.isConfig("https://messages.google.com/accounts/SetOSID"))
        assertEquals("navigation_blocked:example.com/a", NavigationAllowlist.blockedCode("https://example.com/a?value=x"))
    }
}
