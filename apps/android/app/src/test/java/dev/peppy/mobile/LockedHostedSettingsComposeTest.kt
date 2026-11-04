package dev.peppy.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class LockedHostedSettingsComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun lockedHostedSettingsShowsOnlySafeAccountRows() {
        compose.setContent {
            MaterialTheme { LockedHostedSettings("https://peppy.pro", isPlay = true, onOpenSubscription = {}) }
        }

        compose.onNodeWithTag("settings-subscription-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-delete-row").fetchSemanticsNode()
        compose.onNodeWithTag("vault-delete-button").assertDoesNotExist()
        compose.onNodeWithTag("account-error").assertDoesNotExist()
    }
}
