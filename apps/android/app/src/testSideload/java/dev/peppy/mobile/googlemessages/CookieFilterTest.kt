package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CookieFilterTest {
    @Test fun `keeps six required cookies and optional psidts without values in text`() {
        val header = "OSID=unique_value; SID=b; HSID=c; SSID=d; APISID=e; SAPISID=f; __Secure-1PSIDTS=g; ignored=h"
        val filtered = CookieFilter.required(header)

        assertEquals(CookieFilter.names, filtered.values.keys)
        assertFalse(filtered.toString().contains("unique_value"))
        assertTrue(CookieFilter.required("OSID=a; SID=b; HSID=c; SSID=d; APISID=e; SAPISID=f").values.containsKey("SID"))
    }

    @Test fun `refuses duplicate or missing required cookies`() {
        assertTrue(runCatching { CookieFilter.required("OSID=a") }.isFailure)
        assertTrue(runCatching { CookieFilter.required("OSID=a; SID=b; SID=c; HSID=d; SSID=e; APISID=f; SAPISID=g") }.isFailure)
    }
}
