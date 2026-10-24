package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.material3.Switch
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.peppy.mobile.ui.InfoButton
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class InfoButtonTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun opensSuppliedHelpAndDismissesWithoutChangingNearbySwitchState() {
        var enabled by mutableStateOf(false)

        compose.setContent {
            androidx.compose.foundation.layout.Row {
                InfoButton(
                    topic = "SMS history",
                    title = "SMS history",
                    body = "Full history help.",
                    tag = "info-sms-history",
                )
                Switch(checked = enabled, onCheckedChange = { enabled = it })
            }
        }

        compose.onNodeWithTag("info-sms-history").performClick()
        compose.onNodeWithTag("info-sms-history-dialog").assertIsDisplayed()
        compose.onNodeWithText("Full history help.").assertIsDisplayed()
        assertFalse(enabled)
        compose.onNodeWithTag("info-sms-history-dismiss").performClick()
        compose.onNodeWithTag("info-sms-history-dialog").assertDoesNotExist()
        assertFalse(enabled)
    }
}
