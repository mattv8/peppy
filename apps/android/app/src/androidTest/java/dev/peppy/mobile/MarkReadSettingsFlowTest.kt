package dev.peppy.mobile

import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.waitUntilDoesNotExist
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.peppy.mobile.notifications.NotificationMirroringSettings
import dev.peppy.mobile.notifications.NotificationMirroringSettingsSeams
import dev.peppy.mobile.ui.theme.PeppyTheme
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Mark-as-read row with the Rust-owned setting, OS access, and mirroring state replaced by fakes. */
@RunWith(AndroidJUnit4::class)
@OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
class MarkReadSettingsFlowTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val afterChanged = AtomicInteger()

    // Real BACK key dismissal stays on device; the other methods run in the JVM suite.
    @Test
    fun declineAndBackLeaveMarkReadOffAndWriteNothing() {
        val store = FakeMarkReadStore(value = false)
        setTab(store)

        openConsentSheet()
        compose.onNodeWithTag(SHEET_DISMISS).performClick()
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        compose.onNodeWithTag(ROW_TAG).assertIsOff()

        openConsentSheet()
        InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        compose.onNodeWithTag(ROW_TAG).assertIsOff()

        assertEquals(emptyList<Boolean>(), store.writes.toList())
        assertEquals(0, afterChanged.get())
    }

    private fun setTab(
        store: FakeMarkReadStore,
        mirroring: Boolean = true,
        access: Boolean = true,
        sdkInt: Int = 34,
    ) {
        val seams = NotificationMirroringSettingsSeams(
            sdkInt = sdkInt,
            mirroringEnabled = { mirroring },
            accessGranted = { access },
            markReadEnabled = { store.read() },
            setMarkReadEnabled = { _, value -> store.write(value) },
            afterMarkReadChanged = { afterChanged.incrementAndGet() },
        )
        compose.setContent {
            PeppyTheme {
                NotificationMirroringSettings(refresh = 0, onChanged = {}, onOpenAppFilters = {}, seams = seams)
            }
        }
        // The readback must finish before the row shows its real state, so wait for the first read.
        compose.waitUntil(TIMEOUT_MS) { store.reads.get() > 0 }
        compose.waitForIdle()
    }

    private fun openConsentSheet() {
        compose.onNodeWithTag(ROW_TAG).performClick()
        compose.waitUntilExactlyOneExists(hasTestTag(DISCLOSURE_TAG), TIMEOUT_MS)
    }

    /** In-memory stand-in for the Rust-owned setting: readback returns the last write. */
    private class FakeMarkReadStore(value: Boolean?) {
        @Volatile private var value: Boolean? = value
        val reads = AtomicInteger()
        val writes = CopyOnWriteArrayList<Boolean>()

        fun read(): Boolean? {
            reads.incrementAndGet()
            return value
        }

        fun write(next: Boolean) {
            writes += next
            value = next
        }
    }

    private companion object {
        const val TIMEOUT_MS = 10_000L
        const val ROW_TAG = "notification-mirroring-mark-read-row"
        const val SHEET_TAG = "mark-read-consent-sheet"
        const val DISCLOSURE_TAG = "notification-mark-read-disclosure"
        const val SHEET_DISMISS = "mark-read-consent-sheet-dismiss"
    }
}
