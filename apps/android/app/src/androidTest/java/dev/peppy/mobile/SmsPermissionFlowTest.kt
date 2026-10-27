package dev.peppy.mobile

import android.Manifest
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.waitUntilExactlyOneExists
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeFalse
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalTestApi::class)
class SmsPermissionFlowTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test fun inactiveDefaultPolicyOpensEnableConsentRatherThanOffConfirmation() {
        val context = compose.activity
        assumeFalse(smsReadGranted(context))
        assertTrue(GatewayPolicyHost(context).smsCaptureEnabled)
        compose.setContent {
            PeppyTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    SmsTab(context = context, refresh = 0, databaseOpen = false, onChanged = {})
                }
            }
        }
        compose.waitUntilExactlyOneExists(hasTestTag("sms-sync-row") and isEnabled(), 10_000L)
        compose.onNodeWithTag("sms-sync-row").performScrollTo().performClick()

        compose.onNodeWithTag("sms-permissions-disclosure")
            .assertTextEquals(context.getString(R.string.peppy_permissions_sms_disclosure))
        compose.onAllNodesWithTag("sms-off-confirm-sheet").assertCountEquals(0)
        compose.onNodeWithTag("sms-consent-sheet-dismiss").performClick()
        assertTrue(GatewayPolicyHost(context).smsCaptureEnabled)
        assertFalse(granted(context, Manifest.permission.READ_SMS))
    }

    @Test fun decliningMmsConsentDoesNotChangeTheSmsPolicyOrRequestAccess() {
        val context = compose.activity
        assumeFalse(smsReadGranted(context))
        val originalSmsPolicy = GatewayPolicyHost(context).smsCaptureEnabled
        val originalMmsChoice = MmsPreferences(context).enabled
        compose.setContent {
            PeppyTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    MmsSettings(context, 0, false, {})
                }
            }
        }
        compose.onNodeWithTag("mms-enable-row").performScrollTo().performClick()
        compose.onNodeWithTag("mms-capture-disclosure")
            .assertTextEquals(context.getString(R.string.peppy_mms_capture_disclosure))
        compose.onNodeWithTag("mms-consent-sheet-dismiss").performClick()
        org.junit.Assert.assertEquals(originalSmsPolicy, GatewayPolicyHost(context).smsCaptureEnabled)
        org.junit.Assert.assertEquals(originalMmsChoice, MmsPreferences(context).enabled)
        assertFalse(granted(context, Manifest.permission.READ_SMS))
    }
}
