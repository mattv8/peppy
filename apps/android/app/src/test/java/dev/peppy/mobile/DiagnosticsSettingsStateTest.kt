package dev.peppy.mobile

import android.net.Uri
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class DiagnosticsSettingsStateTest {
    @Test
    fun disabledDiagnosticsRetainsExistingLogActions() {
        val actions = diagnosticsActions(DiagnosticsState(enabled = false, bytes = 128, dropped = 3))

        assertFalse(actions.recording)
        assertTrue(actions.canShare)
        assertTrue(actions.canClear)
        assertEquals("128 B", diagnosticsSize(128))
    }

    @Test
    fun emptyDiagnosticsDisablesLogActions() {
        val actions = diagnosticsActions(DiagnosticsState())

        assertFalse(actions.canShare)
        assertFalse(actions.canClear)
        assertEquals("0 B", diagnosticsSize(0))
    }

    @Test
    fun shareIntentGrantsOnlyReadAccessToTheCurrentExport() {
        val uri = Uri.parse("content://dev.peppy.mobile.fileprovider/diagnostics-share/log.txt")
        val intent = diagnosticsShareIntent(uri)

        assertEquals("text/plain", intent.type)
        assertEquals(uri, intent.getParcelableExtra<Uri>(android.content.Intent.EXTRA_STREAM))
        assertEquals(uri, intent.clipData?.getItemAt(0)?.uri)
        assertTrue(intent.flags and android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
        assertEquals(0, intent.flags and android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
    }

    @Test
    fun staleExportDoesNotLaunchChooser() {
        val uri = Uri.parse("content://dev.peppy.mobile.fileprovider/diagnostics-share/log.txt")
        var launches = 0

        launchDiagnosticsShareIfCurrent(uri, isCurrent = { false }) { launches += 1 }

        assertEquals(0, launches)
    }

    @Test
    fun currentExportLaunchesAnAndroidChooser() {
        val uri = Uri.parse("content://dev.peppy.mobile.fileprovider/diagnostics-share/log.txt")
        var chooser: android.content.Intent? = null

        launchDiagnosticsShareIfCurrent(uri, isCurrent = { true }) { chooser = it }

        assertEquals(android.content.Intent.ACTION_CHOOSER, chooser?.action)
        assertEquals(
            android.content.Intent.ACTION_SEND,
            chooser?.getParcelableExtra<android.content.Intent>(android.content.Intent.EXTRA_INTENT)?.action,
        )
    }
}
