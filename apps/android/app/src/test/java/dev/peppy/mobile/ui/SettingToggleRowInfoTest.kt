package dev.peppy.mobile.ui

import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SettingToggleRowInfoTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun infoButtonDoesNotGrowToggleRowBeyondFourDp() {
        compose.setContent {
            MaterialTheme {
                Column {
                    SettingToggleRow(title = "Verbose logging", checked = false, enabled = true, onCheckedChange = {}, tag = "plain", subtitle = "Adds detail")
                    SettingToggleRow(
                        title = "Verbose logging",
                        checked = false,
                        enabled = true,
                        onCheckedChange = {},
                        tag = "with-info",
                        subtitle = "Adds detail",
                        titleTrailing = { InfoButton(topic = "Verbose logging", title = "Verbose logging", body = "Logs more.", tag = "info-verbose") },
                    )
                }
            }
        }
        val plain = compose.onNodeWithTag("plain").fetchSemanticsNode().size.height
        val withInfo = compose.onNodeWithTag("with-info").fetchSemanticsNode().size.height
        val fourDp = ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics.density * 4
        assertTrue("plain=$plain withInfo=$withInfo", withInfo <= plain + fourDp)
    }

    @Test fun infoButtonInToggleRowStaysTappableOutsideItsFootprint() {
        var toggled = false
        compose.setContent {
            MaterialTheme {
                SettingToggleRow(
                    title = "Wait for Wi-Fi",
                    checked = false,
                    enabled = true,
                    onCheckedChange = { toggled = true },
                    tag = "wifi",
                    titleTrailing = {
                        InfoButton(topic = "Wait for Wi-Fi", title = "Wait for Wi-Fi", body = "Waits for Wi-Fi.", tag = "info-y")
                    },
                )
            }
        }
        compose.onNodeWithTag("info-y").performTouchInput { click(Offset(width / 2f, 2f)) }
        compose.onNodeWithTag("info-y-dialog").fetchSemanticsNode()
        compose.runOnIdle { assertFalse(toggled) }
    }

    @Test fun infoButtonInTrailingOpensDialogWithoutTogglingTheSwitch() {
        var toggled = false
        compose.setContent {
            MaterialTheme {
                SettingToggleRow(
                    title = "Wait for Wi-Fi",
                    checked = false,
                    enabled = true,
                    onCheckedChange = { toggled = true },
                    tag = "wifi",
                    titleTrailing = {
                        InfoButton(topic = "Wait for Wi-Fi", title = "Wait for Wi-Fi", body = "Waits for Wi-Fi.", tag = "info-x")
                    },
                )
            }
        }
        compose.onNodeWithTag("info-x").assertHasClickAction()
        compose.onNodeWithTag("info-x").performClick()
        compose.onNodeWithTag("info-x-dialog").fetchSemanticsNode()
        compose.runOnIdle { assertFalse(toggled) }
    }
}
