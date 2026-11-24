package dev.peppy.mobile

import android.content.Context
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.ConsentSheet
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PermissionDisclosureControlsTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

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
                    body = ApplicationProvider.getApplicationContext<Context>().getString(R.string.peppy_permissions_sms_disclosure),
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
