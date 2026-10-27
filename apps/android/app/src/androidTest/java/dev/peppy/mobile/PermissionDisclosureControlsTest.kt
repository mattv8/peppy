package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PermissionDisclosureControlsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun consentSheetDoesNotRequestUntilAgreeAtNormalFont() {
        assertConsentSheet(fontScale = 1f)
    }

    @Test
    fun consentSheetKeepsActionsReachableAtLargeFont() {
        assertConsentSheet(fontScale = 1.3f)
    }

    private fun assertConsentSheet(fontScale: Float) {
        var confirms = 0
        var dismisses = 0

        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                dev.peppy.mobile.ui.ConsentSheet(
                    title = "Allow SMS sync?",
                    body = compose.activity.getString(R.string.peppy_permissions_sms_disclosure),
                    tag = "sms-consent-sheet",
                    onDismiss = { dismisses += 1 },
                    onConfirm = { confirms += 1 },
                    confirmLabel = "Agree and continue",
                    disclosureTag = "sms-permissions-disclosure",
                )
            }
        }

        compose.onNodeWithTag("sms-permissions-disclosure").assertIsDisplayed()
        compose.onNodeWithTag("sms-consent-sheet-dismiss").performClick()
        assertEquals(0, confirms)
        assertEquals(1, dismisses)
        compose.onNodeWithTag("sms-consent-sheet-confirm").performClick()
        compose.onNodeWithTag("sms-consent-sheet-confirm").performClick()
        assertEquals(1, confirms)
    }
}
