package dev.peppy.mobile

import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.waitUntilDoesNotExist
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assume.assumeFalse
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Real Contacts tab wiring: switch -> required read-consent sheet. Decline or Back must leave
 * the consent intent UNSET and must not mark a READ_CONTACTS request as made, which is written
 * only immediately before the OS permission launcher starts.
 */
@RunWith(AndroidJUnit4::class)
@OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
class ContactPermissionFlowTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Before
    fun requireUnconsentedContactsState() {
        val context = compose.activity
        assumeTrue("Precondition: contact sync intent must be UNSET", ContactSyncPreferences(context).intent == ContactSyncIntent.UNSET)
        assumeFalse("Precondition: READ_CONTACTS already granted", ContactSyncHost.access(context).read)
    }

    @Test
    fun switchOpensFullContactsDescriptionBeforeAnyPermissionRequest() {
        setContactsTab()

        openConsentSheet()

        compose.onNodeWithTag(DISCLOSURE_TAG).assertTextEquals(compose.activity.getString(R.string.peppy_contacts_description))
        compose.onNodeWithTag(NEXT_STEP_TAG).assertTextEquals(compose.activity.getString(R.string.peppy_toggle_android_contacts_remote_edit_next))
        assertNoConsentWritten()
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
    }

    @Test
    fun declineLeavesContactSyncOffWithoutPermissionRequest() {
        setContactsTab()

        openConsentSheet()
        compose.onNodeWithTag(SHEET_DISMISS).performClick()
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)

        assertNoConsentWritten()
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
        compose.onAllNodesWithTag(RETIRE_TAG).assertCountEquals(0)
    }

    @Test
    fun backLeavesContactSyncOffWithoutPermissionRequest() {
        setContactsTab()

        openConsentSheet()
        InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)

        assertNoConsentWritten()
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
        compose.onAllNodesWithTag(RETIRE_TAG).assertCountEquals(0)
    }

    private fun setContactsTab() {
        compose.setContent {
            PeppyTheme {
                ContactSyncSettings(context = compose.activity, refresh = 0, onChanged = {})
            }
        }
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
    }

    private fun openConsentSheet() {
        compose.onNodeWithTag(ROW_TAG).performClick()
        compose.waitUntilExactlyOneExists(hasTestTag(DISCLOSURE_TAG), TIMEOUT_MS)
    }

    /** Readback of local consent state: no enable intent and no READ_CONTACTS request recorded. */
    private fun assertNoConsentWritten() {
        val prefs = ContactSyncPreferences(compose.activity)
        assertEquals(ContactSyncIntent.UNSET, prefs.intent)
        assertFalse(prefs.readPermissionRequested)
        assertFalse(ContactSyncHost.access(compose.activity).read)
    }

    private companion object {
        const val TIMEOUT_MS = 10_000L
        const val ROW_TAG = "contact-sync-row"
        const val SHEET_TAG = "contacts-consent-sheet"
        const val DISCLOSURE_TAG = "contact-sync-description"
        const val NEXT_STEP_TAG = "contacts-consent-sheet-next-step"
        const val SHEET_DISMISS = "contacts-consent-sheet-dismiss"
        const val RETIRE_TAG = "contact-retire-confirmation"
    }
}
