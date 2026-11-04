package dev.peppy.mobile.restrictedsetup.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.performClick
import dev.peppy.mobile.restrictedsetup.RestrictedSetupAccess
import dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery
import dev.peppy.mobile.restrictedsetup.RestrictedSetupStage
import dev.peppy.mobile.restrictedsetup.RestrictedSetupState
import dev.peppy.mobile.restrictedsetup.WirelessDebuggingOutcome
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Rule
import org.junit.Test
import org.junit.Assert.assertTrue
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RestrictedSetupPanelComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun advancedFeaturesCardShowsCapabilitiesAndSetupAction() = render(RestrictedSetupState(RestrictedSetupStage.INTRODUCTION)) {
        compose.onNodeWithTag("restricted-setup-idle").fetchSemanticsNode()
        compose.onNodeWithText("Restricted message access").fetchSemanticsNode()
        compose.onNodeWithText("Honor blocked senders").fetchSemanticsNode()
        compose.onNodeWithTag("advanced-features-start").performClick()
        compose.onNodeWithTag("restricted-setup-consent-sheet").fetchSemanticsNode()
        compose.onNodeWithText("appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow", substring = true).fetchSemanticsNode()
        compose.onNodeWithText("appops set dev.peppy.mobile WRITE_SMS allow", substring = true).fetchSemanticsNode()
        compose.onNodeWithText("Android has no read-only version of the blocked-senders setting.", substring = true).fetchSemanticsNode()
    }

    @Test fun advancedFeaturesCardUsesFinishActionForPartialAccess() = render(
        RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, access = RestrictedSetupAccess.ALLOWED),
    ) {
        compose.onNodeWithText("Finish setup").fetchSemanticsNode()
        compose.onNodeWithText("One of two features is on.").fetchSemanticsNode()
        assertTrue(compose.onAllNodesWithText("Advanced features").fetchSemanticsNodes().size == 1)
    }

    @Test fun advancedFeaturesCardHidesWhenAllCapabilitiesAreOn() = render(
        RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, RestrictedSetupAccess.ALLOWED, RestrictedSetupAccess.ALLOWED),
    ) {
        assertMissing("restricted-setup-idle")
        compose.onNodeWithTag("advanced-features-on-card").fetchSemanticsNode()
        compose.onNodeWithText("Both features are on.").fetchSemanticsNode()
    }

    @Test fun onCardDoesNotClaimBothFeaturesWhenOneIsUnsupported() = render(
        RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, RestrictedSetupAccess.UNSUPPORTED, RestrictedSetupAccess.ALLOWED),
    ) {
        compose.onNodeWithTag("advanced-features-on-card").fetchSemanticsNode()
        assertTrue(compose.onAllNodesWithText("Both features are on.").fetchSemanticsNodes().isEmpty())
    }

    @Test fun advancedFeaturesCardHidesWhenAllCapabilitiesAreUnsupported() = render(
        RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, RestrictedSetupAccess.UNSUPPORTED, RestrictedSetupAccess.UNSUPPORTED),
    ) { assertMissing("restricted-setup-idle") }

    @Test fun advancedFeaturesCardExplainsUnsupportedCapabilityBeforeAction() = render(
        RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, RestrictedSetupAccess.UNSUPPORTED, RestrictedSetupAccess.NOT_ALLOWED),
    ) {
        compose.onNodeWithText("This Android version does not offer one of these settings. The other can still be turned on.").fetchSemanticsNode()
        compose.onNodeWithText("Set up advanced features").fetchSemanticsNode()
    }

    @Test fun pairingUsesNotificationOrFallback() = render(RestrictedSetupState(RestrictedSetupStage.AWAITING_PAIRING, discovery = RestrictedSetupDiscovery.FOUND, pairingPort = 37123, notificationAvailable = true)) { compose.onNodeWithTag("restricted-setup-pairing").fetchSemanticsNode(); compose.onNodeWithTag("restricted-setup-notification-resend").fetchSemanticsNode() }

    @Test fun pairingShowsNetworkNotAllowedHint() = render(RestrictedSetupState(RestrictedSetupStage.AWAITING_PAIRING, discovery = RestrictedSetupDiscovery.TIMED_OUT, notificationAvailable = true, networkNotAllowed = true)) { compose.onNodeWithText("Android did not allow Wireless debugging on this Wi-Fi. In Developer options, turn Wireless debugging off and on again, then tap Allow.").fetchSemanticsNode(); assertMissing("restricted-setup-discovery-status"); assertMissing("restricted-setup-code-fallback") }

    @Test fun progressAndConnectPortRender() = render(RestrictedSetupState(RestrictedSetupStage.CONNECTING, discovery = RestrictedSetupDiscovery.TIMED_OUT)) { compose.onNodeWithTag("restricted-setup-progress").fetchSemanticsNode(); compose.onNodeWithTag("restricted-setup-connect-port").fetchSemanticsNode() }

    @Test fun completeRenders() = render(RestrictedSetupState(RestrictedSetupStage.COMPLETE, access = RestrictedSetupAccess.ALLOWED, wirelessDebugging = WirelessDebuggingOutcome.OFF)) { compose.onNodeWithTag("restricted-setup-complete").fetchSemanticsNode() }

    @Test fun completeKeepsWirelessDebuggingSettingsActionWhenTurnOffWasNotRequested() = render(RestrictedSetupState(RestrictedSetupStage.COMPLETE, access = RestrictedSetupAccess.ALLOWED, wirelessDebugging = WirelessDebuggingOutcome.NOT_REQUESTED)) { compose.onNodeWithTag("restricted-setup-complete-open-settings").fetchSemanticsNode() }

    @Test fun completeHidesWirelessDebuggingSettingsActionWhenTurnOffWasRequested() = render(RestrictedSetupState(RestrictedSetupStage.COMPLETE, access = RestrictedSetupAccess.ALLOWED, wirelessDebugging = WirelessDebuggingOutcome.REQUESTED)) { assertMissing("restricted-setup-complete-open-settings") }

    @Test fun blockedRetryHidesStartOver() = render(RestrictedSetupState(RestrictedSetupStage.FAILED, failure = dev.peppy.mobile.restrictedsetup.RestrictedSetupFailure.TRANSPORT, retryBlocked = true)) { assertMissing("restricted-setup-retry") }

    @Test fun transportFailureWithoutBlockedRetryKeepsStartOver() = render(RestrictedSetupState(RestrictedSetupStage.FAILED, failure = dev.peppy.mobile.restrictedsetup.RestrictedSetupFailure.TRANSPORT)) { compose.onNodeWithTag("restricted-setup-retry").assertExists() }

    private fun render(state: RestrictedSetupState, verify: () -> Unit) {
        compose.setContent { MaterialTheme { RestrictedSetupPanel(state, {}, begin = { false }, submitCode = { _, _ -> dev.peppy.mobile.restrictedsetup.CodeSubmitResult.NOT_WAITING }, submitConnectPort = { false }, showCodePromptAgain = {}, cancel = {}, startService = {}) } }
        verify()
    }
    private fun assertMissing(tag: String) = assertTrue(compose.onAllNodesWithTag(tag).fetchSemanticsNodes().isEmpty())
}
