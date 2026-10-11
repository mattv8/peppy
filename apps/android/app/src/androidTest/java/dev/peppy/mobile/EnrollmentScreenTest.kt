package dev.peppy.mobile

import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import org.junit.Rule
import org.junit.Test

class EnrollmentScreenTest {
    @get:Rule val compose = createComposeRule()

    @Test fun scanQrFromInitialServerStepOpensPhoneScanner() {
        compose.setContent {
            EnrollmentScreen(
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
            )
        }

        compose.onNodeWithTag("scan-phone-qr").performClick()
        compose.onNodeWithTag("fake-enrollment-scanner").assertIsDisplayed()
    }

    @Test fun hostedExistingEnrollmentStartsAtThePhoneScanner() {
        compose.setContent {
            EnrollmentScreen(
                initialOrigin = HostedAccountClient.ORIGIN,
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
            )
        }

        compose.onNodeWithTag("fake-enrollment-scanner").assertIsDisplayed()
    }

    @Test fun invalidServerOriginAnnouncesErrorAssertively() {
        compose.setContent {
            EnrollmentScreen(
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
            )
        }

        compose.onNodeWithTag("enrollment-discover").performClick()
        compose.onNodeWithTag("enrollment-error")
            .assertIsDisplayed()
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Assertive))
    }

    @Test fun generatedSecretControlsHideRevealAndAcknowledgeWithoutChangingTheSecret() {
        val secret = "amber birch cedar dune ember frost"
        compose.setContent {
            var revealSecret by remember { mutableStateOf(false) }
            var saved by remember { mutableStateOf(false) }
            GeneratedSecretControls(
                secret = secret,
                revealSecret = revealSecret,
                saved = saved,
                onToggleReveal = { revealSecret = !revealSecret },
                onAcknowledge = { saved = it },
            )
        }

        compose.onNodeWithTag("vault-secret-masked").assertIsDisplayed()
        compose.onNodeWithText(secret).assertDoesNotExist()
        compose.onNodeWithTag("vault-secret-reveal").performClick()
        compose.onNodeWithText(secret).assertIsDisplayed()
        compose.onNodeWithTag("vault-secret-reveal").performClick()
        compose.onNodeWithText(secret).assertDoesNotExist()
        compose.onNodeWithTag("vault-secret-ack").assertIsOff().performClick().assertIsOn()
    }
}
