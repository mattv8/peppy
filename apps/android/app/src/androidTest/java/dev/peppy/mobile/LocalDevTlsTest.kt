package dev.peppy.mobile

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL
import javax.net.ssl.HttpsURLConnection

/**
 * Live TLS check for the debug development origin. It runs only when the operator passes
 * `-Pandroid.testInstrumentationRunnerArguments.peppyDevTlsOrigin=https://10.0.2.2:<port>`
 * (or the equivalent `am instrument -e` argument), so the ordinary smoke run is unchanged.
 *
 * The request uses the platform default TLS stack and the app's own network security config.
 * It installs no trust manager, hostname verifier, or certificate, and follows no redirects.
 */
@RunWith(AndroidJUnit4::class)
class LocalDevTlsTest {
    @Test
    fun debugDevelopmentOriginServesHealthzOverTrustedTls() {
        val origin = InstrumentationRegistry.getArguments().getString(ARGUMENT_NAME)
        assumeTrue("Set $ARGUMENT_NAME to run the live development TLS check", !origin.isNullOrBlank())
        if (!BuildConfig.DEBUG) {
            fail("$ARGUMENT_NAME is only valid for debug builds, which own the development trust anchor")
        }

        val healthz = healthzUrl(requireNotNull(origin))
        val connection = URL(healthz).openConnection() as HttpsURLConnection
        try {
            connection.connectTimeout = TIMEOUT_MILLIS
            connection.readTimeout = TIMEOUT_MILLIS
            connection.instanceFollowRedirects = false
            connection.requestMethod = "GET"
            connection.connect()
            assertEquals(HttpURLConnection.HTTP_OK, connection.responseCode)
        } finally {
            connection.disconnect()
        }
    }

    private fun healthzUrl(origin: String): String {
        val uri = URI(origin)
        val host = uri.host.orEmpty().lowercase()
        assertEquals("origin must use https", "https", uri.scheme?.lowercase())
        assertTrue("origin host must be an explicit loopback host, got '$host'", host in LOOPBACK_HOSTS)
        assertTrue("origin must not include userinfo", uri.rawUserInfo == null)
        assertTrue("origin must not include a query or fragment", uri.rawQuery == null && uri.rawFragment == null)
        assertTrue("origin path must be empty or '/'", uri.rawPath.isNullOrEmpty() || uri.rawPath == "/")
        return "https://${uri.rawAuthority}/healthz"
    }

    private companion object {
        const val ARGUMENT_NAME = "peppyDevTlsOrigin"
        const val TIMEOUT_MILLIS = 5_000
        val LOOPBACK_HOSTS = setOf("localhost", "127.0.0.1", "[::1]", "::1", "10.0.2.2")
    }
}
