package dev.peppy.mobile.googlemessages

import java.nio.file.Files
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class WebViewDataFilesTest {
    @Test fun `deletes the complete feature-owned WebView profile and cache`() {
        val root = Files.createTempDirectory("webview-data").toFile()
        val directory = root.resolve("app_webview/Default").also { it.mkdirs() }
        val cache = Files.createTempDirectory("webview-cache").toFile().resolve("WebView").also { it.mkdirs() }
        val chromiumCache = cache.parentFile!!.resolve("org.chromium.android_webview").also { it.mkdirs() }
        val unrelated = root.resolve("unrelated-feature").also { it.writeText("keep") }
        listOf("Cookies", "History", "Local Storage", "IndexedDB", "Service Worker").forEach { name ->
            directory.resolve(name).writeText("x")
        }

        assertTrue(WebViewDataFiles.deleteStaleData(root, cache.parentFile!!))
        assertFalse(directory.exists())
        assertFalse(cache.exists())
        assertFalse(chromiumCache.exists())
        assertTrue(unrelated.exists())
    }
}
