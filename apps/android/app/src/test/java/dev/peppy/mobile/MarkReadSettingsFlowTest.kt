package dev.peppy.mobile

import android.content.Context
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.isNotEnabled
import androidx.compose.ui.test.isOff
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.waitUntilDoesNotExist
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.notifications.NotificationMirroringSettings
import dev.peppy.mobile.notifications.NotificationMirroringSettingsSeams
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.theme.PeppyTheme
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Mark-as-read row in the Mirroring tab, with the Rust-owned setting, OS access, and mirroring
 * state replaced by fakes. Asserts the UI contract only: consent before the first write, the row
 * usable only after readback, decline writes nothing, and turning off never needs the sheet.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi")
@OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
class MarkReadSettingsFlowTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    private val afterChanged = AtomicInteger()

    @Test
    fun turningOnShowsConsentAndAgreeEnablesAfterReadback() {
        val store = FakeMarkReadStore(value = false)
        setTab(store)

        compose.onNodeWithTag(ROW_TAG).performClick()
        compose.waitUntilExactlyOneExists(hasTestTag(DISCLOSURE_TAG), TIMEOUT_MS)
        compose.onNodeWithTag(DISCLOSURE_TAG).assertTextEquals(string(R.string.peppy_notification_mark_read_disclosure))
        assertEquals(emptyList<Boolean>(), store.writes.toList())

        compose.onNodeWithTag(SHEET_CONFIRM).performClick()
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)

        compose.onNodeWithTag(ROW_TAG).assertIsOn()
        assertEquals(listOf(true), store.writes.toList())
        assertEquals(1, afterChanged.get())
    }

    @Test
    fun turningOffAppliesWithoutConsentSheet() {
        val store = FakeMarkReadStore(value = true)
        setTab(store)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        compose.onNodeWithTag(ROW_TAG).assertIsOn()

        compose.onNodeWithTag(ROW_TAG).performClick()
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)

        compose.onNodeWithTag(ROW_TAG).assertIsOff()
        compose.onNodeWithTag(SHEET_TAG).assertDoesNotExist()
        assertEquals(listOf(false), store.writes.toList())
    }

    @Test
    fun storedOnStaysRevocableWhenMirroringIsOff() {
        val store = FakeMarkReadStore(value = true)
        setTab(store, mirroring = false)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        compose.onNodeWithTag(ROW_TAG).performClick()
        // Once off, the paused prerequisites disable the row again; turning it back on needs them.
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isOff() and isNotEnabled(), TIMEOUT_MS)

        assertEquals(listOf(false), store.writes.toList())
    }

    @Test
    fun rowIsDisabledWithMirroringReasonWhenMirroringIsOff() {
        val store = FakeMarkReadStore(value = false)
        setTab(store, mirroring = false)

        awaitDisabledReason(string(R.string.peppy_toggle_android_mark_read_needs_mirroring))
        assertEquals(emptyList<Boolean>(), store.writes.toList())
    }

    @Test
    fun rowIsDisabledWithAccessReasonWhenAccessIsMissing() {
        val store = FakeMarkReadStore(value = false)
        setTab(store, access = false)

        awaitDisabledReason(string(R.string.peppy_toggle_android_mark_read_needs_access))
    }

    @Test
    fun rowIsDisabledWithAndroid9ReasonBelowAndroid9() {
        val store = FakeMarkReadStore(value = false)
        setTab(store, sdkInt = 26)

        awaitDisabledReason(string(R.string.peppy_toggle_android_mark_read_needs_android_9))
        assertEquals(emptyList<Boolean>(), store.writes.toList())
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

    /**
     * Waits for the exact reason, so the row is read only after its state has loaded. The row's
     * toggleable merges its subtitle, so the reason is matched on the merged row node.
     */
    private fun awaitDisabledReason(reason: String) {
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and hasText(reason, substring = true), TIMEOUT_MS)
        compose.onNodeWithTag(ROW_TAG).assertIsNotEnabled()
    }

    private fun string(id: Int): String = ApplicationProvider.getApplicationContext<Context>().getString(id)

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
        const val SHEET_CONFIRM = "mark-read-consent-sheet-confirm"
    }
}
