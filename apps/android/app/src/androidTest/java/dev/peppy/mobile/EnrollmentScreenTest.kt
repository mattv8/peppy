package dev.peppy.mobile

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
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
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
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
                scanner = { _, _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
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
                scanner = { _, _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
            )
        }

        compose.onNodeWithTag("fake-enrollment-scanner").assertIsDisplayed()
    }

    @Test fun failedScanReplacesScannerWithRetryAndCanScanAgain() {
        var onCode: ((String) -> Unit)? = null
        compose.setContent {
            EnrollmentScreen(
                initialOrigin = HostedAccountClient.ORIGIN,
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { modifier, callback, _ ->
                    onCode = callback
                    Box(modifier.fillMaxSize())
                },
            )
        }

        compose.onNodeWithTag("enrollment-qr-scanner").assertIsDisplayed()
        compose.onNodeWithTag("enrollment-cancel").assertIsDisplayed()
        compose.runOnIdle { onCode?.invoke("{") }
        compose.runOnIdle { onCode?.invoke("{") }
        compose.waitUntil(5_000) {
            compose.onAllNodesWithTag("phone-terminal-error").fetchSemanticsNodes().isNotEmpty()
        }

        compose.onNodeWithTag("phone-terminal-error").assertIsDisplayed()
        compose.onNodeWithTag("phone-retry").assertIsDisplayed()
        compose.onNodeWithTag("enrollment-qr-scanner").assertDoesNotExist()
        compose.runOnIdle { onCode?.invoke("{") }
        compose.onNodeWithTag("phone-terminal-error").assertIsDisplayed()
        compose.onNodeWithTag("phone-retry").performClick()
        compose.onNodeWithTag("phone-terminal-error").assertDoesNotExist()
        compose.onNodeWithTag("enrollment-qr-scanner").assertIsDisplayed()
        compose.runOnIdle { onCode?.invoke("{") }
        compose.waitUntil(5_000) {
            compose.onAllNodesWithTag("phone-terminal-error").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithTag("phone-terminal-error").assertIsDisplayed()
    }

    @Test fun cancelFromPhoneScannerDispatchesDismiss() {
        var dismisses = 0
        compose.setContent {
            EnrollmentScreen(
                initialOrigin = HostedAccountClient.ORIGIN,
                onDismiss = { dismisses += 1 },
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { modifier, _, _ -> Box(modifier.fillMaxSize()) },
            )
        }

        compose.onNodeWithTag("enrollment-cancel").assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(1, dismisses) }
    }

    @Test fun unavailableCameraFromPhoneScannerShowsTerminalWithoutAutomaticRetry() {
        var onUnavailable: (() -> Unit)? = null
        compose.setContent {
            EnrollmentScreen(
                initialOrigin = HostedAccountClient.ORIGIN,
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { modifier, _, unavailable ->
                    onUnavailable = unavailable
                    Box(modifier.fillMaxSize())
                },
            )
        }

        compose.onNodeWithTag("enrollment-qr-scanner").assertIsDisplayed()
        compose.runOnIdle { onUnavailable?.invoke() }
        compose.onNodeWithTag("phone-terminal-error")
            .assertIsDisplayed()
            .assertTextEquals(InstrumentationRegistry.getInstrumentation().targetContext.getString(R.string.peppy_enrollment_camera_unavailable))
        compose.onNodeWithTag("phone-retry").assertIsDisplayed()
        compose.onNodeWithTag("enrollment-cancel").assertIsDisplayed()
        compose.onNodeWithTag("enrollment-qr-scanner").assertDoesNotExist()
        compose.waitForIdle()
        compose.onNodeWithTag("enrollment-qr-scanner").assertDoesNotExist()
    }

    @Test fun invalidServerOriginAnnouncesErrorAssertively() {
        compose.setContent {
            EnrollmentScreen(
                onDismiss = {},
                onEnrolled = {},
                hasCameraPermission = { true },
                scanner = { _, _, _ -> Text("scanner", Modifier.testTag("fake-enrollment-scanner")) },
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
