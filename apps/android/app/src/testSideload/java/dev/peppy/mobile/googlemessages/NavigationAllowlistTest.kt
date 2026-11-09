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

    @Test fun `blocks only unpermitted Messages subresources`() {
        listOf(
            "https://www.gstatic.com/signin/v2/slid.css",
            "https://ssl.gstatic.com/accounts/v1.js",
            "https://fonts.gstatic.com/s/font.woff2",
            "https://apis.google.com/js/api.js",
            "https://play.google.com/log",
            "https://accounts.google.com/gsi/client",
            "https://messages.google.com/web/config?pli=1",
            "https://messages.google.com/accounts/SetOSID",
        ).forEach { assertFalse(NavigationAllowlist.blocksSubresource(it)) }

        listOf(
            "https://messages.google.com/web/conversations",
            "http://messages.google.com/web/config",
            "HTTPS://MESSAGES.GOOGLE.COM/web/conversations",
            "https://messages.google.com./web/conversations",
            "https://messages.google.com/invalid path",
        ).forEach { assertTrue(NavigationAllowlist.blocksSubresource(it)) }

        assertFalse(NavigationAllowlist.blocksSubresource("https://example.com/invalid path"))
    }
}
