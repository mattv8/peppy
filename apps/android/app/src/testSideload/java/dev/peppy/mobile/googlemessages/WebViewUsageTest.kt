package dev.peppy.mobile.googlemessages

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Test

class WebViewUsageTest {
    @Test fun `sign in activity is the only WebView user`() {
        val sourceRoot = File("src").absoluteFile
        val users = sourceRoot.walkTopDown()
            .filter { it.extension == "kt" && it.readText().contains("android.webkit." + "WebView") }
            .map { it.relativeTo(sourceRoot).invariantSeparatorsPath }
            .toList()

        assertEquals(listOf("gmlink/java/dev/peppy/mobile/googlemessages/LinkSignInActivity.kt"), users)
    }
}
