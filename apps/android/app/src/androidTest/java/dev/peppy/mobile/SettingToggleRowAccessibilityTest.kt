package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class SettingToggleRowAccessibilityTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test fun settingsActionHasALabelAndDoesNotInventAnOsStateChange() {
        var requests = 0
        compose.setContent {
            PeppyTheme {
                SettingToggleRow(
                    title = "Background sync",
                    checked = false,
                    enabled = true,
                    onCheckedChange = { requests++ },
                    tag = "battery-action",
                    onClickLabel = "Open Android settings",
                    stateDescription = "Managed by Android",
                )
            }
        }

        val row = compose.onNodeWithTag("battery-action")
        val semantics = row.fetchSemanticsNode().config
        assertEquals("Open Android settings", semantics[SemanticsActions.OnClick].label)
        assertEquals("Managed by Android", semantics[SemanticsProperties.StateDescription])
        row.performClick().assertIsOff()
        assertEquals(1, requests)
    }

    @Test fun wrappedSubtitleKeepsAReadableGapBeforeTheSwitch() {
        compose.setContent {
            PeppyTheme {
                SettingToggleRow(
                    title = "Wi-Fi only",
                    subtitle = "Uses WiFi, not mobile data or Ethernet. The current network controls when sync can run.",
                    checked = false,
                    enabled = true,
                    onCheckedChange = {},
                    tag = "wrapped-setting",
                )
            }
        }
        val subtitle = compose.onNodeWithTag("wrapped-setting-subtitle", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val control = compose.onNodeWithTag("wrapped-setting-switch", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val minimumGap = 15f * compose.activity.resources.displayMetrics.density
        assertTrue("Wrapped text must not touch the switch", control.left - subtitle.right >= minimumGap)
    }
}
