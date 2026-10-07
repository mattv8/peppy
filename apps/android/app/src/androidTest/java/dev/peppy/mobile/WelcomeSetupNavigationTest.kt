package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.atomic.AtomicInteger

@RunWith(AndroidJUnit4::class)
class WelcomeSetupNavigationTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun hostedIsDefaultAndGetStartedOpensHostedEnrollment() {
        showWelcomeFlow()

        composeRule.onNodeWithTag("welcome-screen").assertIsDisplayed()
        composeRule.onNodeWithText(
            composeRule.activity.getString(R.string.peppy_onboarding_mode_hosted),
        ).assertIsDisplayed()
        composeRule.onNodeWithTag("welcome-get-started-button").performClick()

        composeRule.onNodeWithTag("hosted-enrollment-destination").assertIsDisplayed()
    }

    @Test
    fun selectingSelfHostedOpensSetupWithoutGetStartedAndHostedSelectionStaysOnWelcome() {
        showWelcomeFlow()

        composeRule.onNodeWithTag("welcome-server-mode-selector").performClick()
        composeRule.onNodeWithTag("welcome-server-mode-hosted").performClick()
        composeRule.onNodeWithTag("welcome-screen").assertIsDisplayed()

        composeRule.onNodeWithTag("welcome-server-mode-selector").performClick()
        composeRule.onNodeWithTag("welcome-server-mode-self-hosted").performClick()

        composeRule.onNodeWithTag("self-hosted-screen").assertIsDisplayed()
    }

    @Test
    fun menuDismissalLeavesHostedWelcomeUnchanged() {
        showWelcomeFlow()

        composeRule.onNodeWithTag("welcome-server-mode-selector").performClick()
        composeRule.onNodeWithTag("welcome-server-mode-self-hosted").assertIsDisplayed()
        composeRule.activity.runOnUiThread {
            composeRule.activity.onBackPressedDispatcher.onBackPressed()
        }
        composeRule.waitForIdle()

        composeRule.onNodeWithTag("welcome-screen").assertIsDisplayed()
        composeRule.onNodeWithTag("welcome-server-mode-self-hosted").assertDoesNotExist()
    }

    @Test
    fun explicitAndSystemBackFromSelfHostedReturnToHostedWelcome() {
        showWelcomeFlow(initialDestination = WelcomeDestination.SELF_HOSTED)

        composeRule.onNodeWithTag("self-hosted-back-button").performClick()
        composeRule.onNodeWithTag("welcome-screen").assertIsDisplayed()

        openSelfHostedSetup()
        composeRule.activity.runOnUiThread {
            composeRule.activity.onBackPressedDispatcher.onBackPressed()
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag("welcome-screen").assertIsDisplayed()
    }

    @Test
    fun pairingCancellationReturnsToItsOrigin() {
        assertEquals(
            WelcomeDestination.SELF_HOSTED,
            pairingCancellationDestination(WelcomeDestination.SELF_HOSTED_PAIRING),
        )
        assertEquals(
            WelcomeDestination.HOSTED,
            pairingCancellationDestination(WelcomeDestination.HOSTED_PAIRING),
        )
    }

    @Test
    fun selectingSelfHostedDoesNotStartPairingOrCredentialImport() {
        val pairingStarts = AtomicInteger()
        val importStarts = AtomicInteger()
        composeRule.setContent {
            var selfHosted by remember { mutableStateOf(false) }
            if (selfHosted) {
                SelfHostedSetupScreen(
                    importBusy = false,
                    importResult = null,
                    onBack = { selfHosted = false },
                    onPair = { pairingStarts.incrementAndGet() },
                    onImport = { importStarts.incrementAndGet() },
                    onOpenGuide = {},
                )
            } else {
                WelcomeScreen(onGetStarted = {}, onSelectSelfHosted = { selfHosted = true })
            }
        }

        composeRule.onNodeWithTag("welcome-server-mode-selector").performClick()
        composeRule.onNodeWithTag("welcome-server-mode-self-hosted").performClick()

        assertEquals(0, pairingStarts.get())
        assertEquals(0, importStarts.get())
    }

    @Test
    fun selfHostedDestinationSurvivesSavedStateRestoration() {
        val restorationTester = StateRestorationTester(composeRule)
        restorationTester.setContent { WelcomeFlow() }

        openSelfHostedSetup()
        restorationTester.emulateSavedInstanceStateRestore()

        composeRule.onNodeWithTag("self-hosted-screen").assertIsDisplayed()
    }

    private fun showWelcomeFlow(initialDestination: WelcomeDestination = WelcomeDestination.HOSTED) {
        composeRule.setContent { WelcomeFlow(initialDestination) }
    }

    private fun openSelfHostedSetup() {
        composeRule.onNodeWithTag("welcome-server-mode-selector").performClick()
        composeRule.onNodeWithTag("welcome-server-mode-self-hosted").performClick()
        composeRule.onNodeWithTag("self-hosted-screen").assertIsDisplayed()
    }
}

@Composable
private fun WelcomeFlow(initialDestination: WelcomeDestination = WelcomeDestination.HOSTED) {
    var destination by rememberSaveable { mutableStateOf(initialDestination) }
    when (destination) {
        WelcomeDestination.HOSTED -> WelcomeScreen(
            onGetStarted = { destination = WelcomeDestination.HOSTED_ENROLLMENT },
            onSelectSelfHosted = { destination = WelcomeDestination.SELF_HOSTED },
        )
        WelcomeDestination.SELF_HOSTED -> SelfHostedSetupScreen(
            importBusy = false,
            importResult = null,
            onBack = { destination = WelcomeDestination.HOSTED },
            onPair = { destination = WelcomeDestination.SELF_HOSTED_PAIRING },
            onImport = {},
            onOpenGuide = {},
        )
        WelcomeDestination.HOSTED_ENROLLMENT -> Text(
            "Hosted enrollment",
            Modifier.testTag("hosted-enrollment-destination"),
        )
        else -> Text("Pairing", Modifier.testTag("pairing-destination"))
    }
}
