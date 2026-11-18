package dev.peppy.mobile.enrollment

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.testing.ComposeTestActivityRule
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class HostedPassphraseSaveComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun acknowledgementLabelTogglesCheckboxAndEnablesContinue() {
        var continued = false
        compose.setPassphraseStep(onContinue = { continued = true })

        compose.onNodeWithTag("passphrase-saved-checkbox").assertIsOff()
        compose.onNodeWithTag("passphrase-continue-button").assertIsNotEnabled()
        val acknowledgement = ApplicationProvider.getApplicationContext<Context>()
            .getString(R.string.peppy_passphrase_ack_label)
        compose.onNodeWithText(acknowledgement).performScrollTo().performClick()
        compose.onNodeWithTag("passphrase-saved-checkbox").assertIsOn()
        compose.onNodeWithTag("passphrase-continue-button").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(true, continued) }
    }

    @Test fun savedPasswordManagerDoesNotAcknowledgePhrase() {
        val saver = PassphrasePasswordSaver { _, _ -> PassphraseSaveOutcome.SAVED }

        compose.setPassphraseStep(saver)

        compose.onNodeWithTag("passphrase-save-password-manager").performScrollTo().performClick()
        compose.onNodeWithTag("passphrase-save-password-manager-saved").assertExists()
        compose.onNodeWithTag("passphrase-saved-checkbox").assertIsOff()
        compose.onNodeWithTag("passphrase-continue-button").assertIsNotEnabled()
    }

    @Test fun cancelledSaveReturnsToAvailableButtonWithoutFailure() {
        val saver = PassphrasePasswordSaver { _, _ -> PassphraseSaveOutcome.CANCELLED }

        compose.setPassphraseStep(saver)

        compose.onNodeWithTag("passphrase-save-password-manager").performScrollTo().performClick()
        compose.onNodeWithTag("passphrase-save-password-manager").assertExists()
        compose.onNodeWithTag("passphrase-save-password-manager-failed").assertDoesNotExist()
    }

    @Test fun failedSaveShowsFailureAndAllowsRetry() {
        val saver = PassphrasePasswordSaver { _, _ -> PassphraseSaveOutcome.FAILED }

        compose.setPassphraseStep(saver)

        compose.onNodeWithTag("passphrase-save-password-manager").performScrollTo().performClick()
        compose.onNodeWithTag("passphrase-save-password-manager-failed").assertExists()
        compose.onNodeWithTag("passphrase-save-password-manager").assertIsEnabled()
        compose.onNodeWithTag("passphrase-save-password-manager").performScrollTo().performClick()
    }

    @Test fun phraseCardShowsNumberedWordsAndFullPhraseDescription() {
        val phrase = "amber birch candle drift ember fern"

        compose.setPassphraseStep(phrase = phrase)

        (1..6).forEach { compose.onNodeWithText("$it.").assertExists() }
        compose.onNodeWithContentDescription(phrase).assertExists()
    }

    private fun androidx.compose.ui.test.junit4.ComposeContentTestRule.setPassphraseStep(
        saver: PassphrasePasswordSaver = PassphrasePasswordSaver { _, _ -> PassphraseSaveOutcome.CANCELLED },
        phrase: String = "amber birch candle drift ember fern",
        onContinue: () -> Unit = {},
    ) {
        setContent {
            var acknowledged by remember { mutableStateOf(false) }
            var saveState by remember { mutableStateOf(PassphraseSaveState.IDLE) }
            MaterialTheme {
                Column(
                    Modifier.verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    HostedPassphraseStep(
                        phrase = phrase,
                        acknowledged = acknowledged,
                        saveState = saveState,
                        onAcknowledgedChange = { acknowledged = it },
                        onSave = {
                            saveState = PassphraseSaveState.SAVING
                            saveState = when (runBlocking { saver.save("Peppy Vault key phrase", phrase) }) {
                                PassphraseSaveOutcome.SAVED -> PassphraseSaveState.SAVED
                                PassphraseSaveOutcome.CANCELLED -> PassphraseSaveState.IDLE
                                PassphraseSaveOutcome.FAILED -> PassphraseSaveState.FAILED
                            }
                        },
                        onContinue = onContinue,
                    )
                }
            }
        }
    }
}
