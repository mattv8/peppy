package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkStartupTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Test fun `startup wipe removes stale cookie and cache data before sign in`() {
        val cookie = context.dataDir.resolve("app_webview/Default/Cookies").also {
            it.parentFile?.mkdirs()
            it.writeText("x")
        }
        val cache = context.cacheDir.resolve("WebView").also {
            it.mkdirs()
            it.resolve("cache").writeText("x")
        }

        assertTrue(GoogleMessagesLink.wipeStaleWebViewData(context))
        assertFalse(cookie.exists())
        assertFalse(cache.exists())
    }
}
