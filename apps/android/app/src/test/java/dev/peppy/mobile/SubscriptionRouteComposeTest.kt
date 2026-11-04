package dev.peppy.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.billing.PlaySubscriptionFeedbackLayout
import dev.peppy.mobile.testing.ComposeTestActivityRule
import androidx.compose.ui.test.performClick
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SubscriptionRouteComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun signInFeedbackRemainsVisibleWithReadySubscriptionContent() {
        compose.setContent {
            MaterialTheme {
                PlaySubscriptionFeedbackLayout(signInError = "Sign-in failed") {
                    Text("Ready", Modifier.testTag("ready-subscription-content"))
                }
            }
        }

        compose.onNodeWithTag("play-signin-error").fetchSemanticsNode()
        compose.onNodeWithTag("ready-subscription-content").fetchSemanticsNode()
    }

    @Test fun appFilterRibbonSubscriptionRouteReplacesAndReturnsToUnderlyingContent() {
        compose.setContent {
            MaterialTheme {
                var subscriptionOpen by remember { mutableStateOf(false) }
                SubscriptionRoute(
                    subscriptionOpen = subscriptionOpen,
                    isPlay = true,
                    subscriptionContent = {
                        TextButton(onClick = { subscriptionOpen = false }, modifier = Modifier.testTag("subscription-back-button")) {
                            Text("Back")
                        }
                        Text("Subscription", Modifier.testTag("subscription-screen"))
                    },
                ) {
                    TextButton(onClick = { subscriptionOpen = true }, modifier = Modifier.testTag("app-filter-ribbon-action")) {
                        Text("Subscription")
                    }
                    Text("Filters", Modifier.testTag("app-filters-content"))
                    Text("Locked", Modifier.testTag("lock-screen"))
                    Text("Settings", Modifier.testTag("settings-screen"))
                    Text("Tabs", Modifier.testTag("bottom-navigation"))
                }
            }
        }

        compose.onNodeWithTag("app-filter-ribbon-action").performClick()
        compose.onNodeWithTag("subscription-screen").fetchSemanticsNode()
        compose.onNodeWithTag("app-filters-content").assertDoesNotExist()
        compose.onNodeWithTag("lock-screen").assertDoesNotExist()
        compose.onNodeWithTag("settings-screen").assertDoesNotExist()
        compose.onNodeWithTag("bottom-navigation").assertDoesNotExist()

        compose.onNodeWithTag("subscription-back-button").performClick()
        compose.onNodeWithTag("app-filters-content").fetchSemanticsNode()
    }
}
