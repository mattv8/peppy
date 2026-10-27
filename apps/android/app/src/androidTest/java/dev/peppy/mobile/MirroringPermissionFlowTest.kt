package dev.peppy.mobile

import android.app.Activity
import android.app.Instrumentation
import android.content.IntentFilter
import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.waitUntilDoesNotExist
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assume.assumeFalse
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Real Mirroring tab wiring: master switch -> required consent sheet -> (agree) exact notification
 * listener settings intent. The settings activity is intercepted and returns CANCELED, so no OS
 * notification access is granted and no mirroring policy is written.
 */
@RunWith(AndroidJUnit4::class)
@OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
class MirroringPermissionFlowTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val instrumentation: Instrumentation = InstrumentationRegistry.getInstrumentation()
    private lateinit var settingsMonitor: Instrumentation.ActivityMonitor

    @Before
    fun interceptNotificationListenerSettings() {
        // Matches the exact action production launches; the blocked activity returns CANCELED.
        settingsMonitor = instrumentation.addMonitor(
            IntentFilter(NotificationMirrorAccess.settingsIntent().action),
            Instrumentation.ActivityResult(Activity.RESULT_CANCELED, null),
            true,
        )
    }

    @After
    fun removeMonitor() {
        instrumentation.removeMonitor(settingsMonitor)
    }

    @Test
    fun masterSwitchOpensFullDisclosureBeforeAnyOsRequest() {
        setMirroringTab()
        val before = policyAndAccessSnapshot()

        openConsentSheet()

        compose.onNodeWithTag(DISCLOSURE_TAG).assertTextEquals(disclosureText())
        compose.onNodeWithTag(SHEET_CONFIRM).assertIsEnabled()
        assertEquals(0, settingsMonitor.hits)
        assertEquals(before, policyAndAccessSnapshot())
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
    }

    @Test
    fun notNowAndBackDismissWithoutOsAccessOrPolicyChange() {
        setMirroringTab()
        val before = policyAndAccessSnapshot()

        openConsentSheet()
        compose.onNodeWithTag(SHEET_DISMISS).performClick()
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        assertEquals(0, settingsMonitor.hits)
        assertEquals(before, policyAndAccessSnapshot())
        compose.onNodeWithTag(ROW_TAG).assertIsOff()

        openConsentSheet()
        instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
        assertEquals(0, settingsMonitor.hits)
        assertEquals(before, policyAndAccessSnapshot())
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
    }

    @Test
    fun agreeLaunchesExactlyOneListenerSettingsIntentAndCancelledResultKeepsMirroringOff() {
        setMirroringTab()
        val before = policyAndAccessSnapshot()

        openConsentSheet()
        compose.onNodeWithTag(SHEET_CONFIRM).performClick()
        compose.waitUntilDoesNotExist(hasTestTag(SHEET_TAG), TIMEOUT_MS)
        compose.waitUntil(TIMEOUT_MS) { settingsMonitor.hits == 1 }
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)

        assertEquals(1, settingsMonitor.hits)
        assertEquals(before, policyAndAccessSnapshot())
        compose.onNodeWithTag(ROW_TAG).assertIsOff()
    }

    private fun setMirroringTab() {
        assumeFalse("Precondition: mirroring already enabled", NotificationMirrorPreferences.enabled(compose.activity))
        assumeFalse("Precondition: notification access already granted", NotificationMirrorAccess.granted(compose.activity))
        compose.setContent {
            PeppyTheme {
                NotificationMirroringSettings(refresh = 0, onChanged = {}, onOpenAppFilters = {})
            }
        }
        compose.waitUntilExactlyOneExists(hasTestTag(ROW_TAG) and isEnabled(), TIMEOUT_MS)
    }

    private fun openConsentSheet() {
        compose.onNodeWithTag(ROW_TAG).performClick()
        compose.waitUntilExactlyOneExists(hasTestTag(DISCLOSURE_TAG), TIMEOUT_MS)
    }

    private fun disclosureText(): String = compose.activity.getString(R.string.peppy_notification_mirroring_disclosure)

    /** Readback of the real stores; any policy write or access change would alter this value. */
    private fun policyAndAccessSnapshot(): Pair<Boolean, Boolean> =
        NotificationMirrorPreferences.enabled(compose.activity) to NotificationMirrorAccess.granted(compose.activity)

    private companion object {
        const val TIMEOUT_MS = 10_000L
        const val ROW_TAG = "notification-mirroring-master-row"
        const val SHEET_TAG = "mirroring-consent-sheet"
        const val DISCLOSURE_TAG = "notification-mirroring-disclosure"
        const val SHEET_CONFIRM = "mirroring-consent-sheet-confirm"
        const val SHEET_DISMISS = "mirroring-consent-sheet-dismiss"
    }
}
