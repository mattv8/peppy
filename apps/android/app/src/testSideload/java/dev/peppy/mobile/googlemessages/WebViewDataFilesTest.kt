package dev.peppy.mobile.googlemessages

import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class WebViewDataFilesTest {
    @Test fun `finds and deletes only cookie database files`() {
        val root = Files.createTempDirectory("webview-data").toFile()
        val directory = root.resolve("app_webview/Default").also { it.mkdirs() }
        val cookie = directory.resolve("Cookies").also { it.writeText("x") }
        val journal = directory.resolve("Cookies-journal").also { it.writeText("x") }
        directory.resolve("History").writeText("x")

        assertEquals(setOf(cookie.canonicalFile, journal.canonicalFile), WebViewDataFiles.cookieDatabases(root).map { it.canonicalFile }.toSet())
        assertTrue(WebViewDataFiles.deleteCookieDatabases(root))
        assertFalse(cookie.exists())
    }
}
