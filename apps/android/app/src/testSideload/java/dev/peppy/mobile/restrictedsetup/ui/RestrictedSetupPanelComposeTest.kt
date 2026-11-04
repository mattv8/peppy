package dev.peppy.mobile.restrictedsetup.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.restrictedsetup.RestrictedSetupAccess
import dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery
import dev.peppy.mobile.restrictedsetup.RestrictedSetupStage
import dev.peppy.mobile.restrictedsetup.RestrictedSetupState
import dev.peppy.mobile.restrictedsetup.WirelessDebuggingOutcome
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RestrictedSetupPanelComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun idleRenders() = render(RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, cleanupNeeded = false)) { compose.onNodeWithTag("restricted-setup-idle").fetchSemanticsNode() }

    @Test fun pairingUsesNotificationOrFallback() = render(RestrictedSetupState(RestrictedSetupStage.AWAITING_PAIRING, cleanupNeeded = false, discovery = RestrictedSetupDiscovery.FOUND, pairingPort = 37123, notificationAvailable = true)) { compose.onNodeWithTag("restricted-setup-pairing").fetchSemanticsNode(); compose.onNodeWithTag("restricted-setup-notification-resend").fetchSemanticsNode() }

    @Test fun progressAndConnectPortRender() = render(RestrictedSetupState(RestrictedSetupStage.CONNECTING, cleanupNeeded = false, discovery = RestrictedSetupDiscovery.TIMED_OUT)) { compose.onNodeWithTag("restricted-setup-progress").fetchSemanticsNode(); compose.onNodeWithTag("restricted-setup-connect-port").fetchSemanticsNode() }

    @Test fun completeRenders() = render(RestrictedSetupState(RestrictedSetupStage.COMPLETE, access = RestrictedSetupAccess.ALLOWED, cleanupNeeded = true, wirelessDebugging = WirelessDebuggingOutcome.OFF)) { compose.onNodeWithTag("restricted-setup-complete").fetchSemanticsNode(); compose.onNodeWithTag("restricted-setup-cleanup").fetchSemanticsNode() }

    @Test fun cleanupRequiresAcknowledgementGates() = render(RestrictedSetupState(RestrictedSetupStage.CLEANUP_REQUIRED, cleanupNeeded = true)) { compose.onNodeWithTag("restricted-setup-cleanup-acknowledge").fetchSemanticsNode() }

    private fun render(state: RestrictedSetupState, verify: () -> Unit) {
        compose.setContent { MaterialTheme { RestrictedSetupPanel(state, {}, begin = { false }, submitCode = { _, _ -> dev.peppy.mobile.restrictedsetup.CodeSubmitResult.NOT_WAITING }, submitConnectPort = { false }, showCodePromptAgain = {}, cancel = {}, acknowledgeCleanup = { false }, startService = {}) } }
        verify()
    }
}
