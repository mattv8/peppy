package dev.peppy.mobile.replyviagm

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import dev.peppy.mobile.R
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.GatewayTestBase
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
import kotlinx.coroutines.runBlocking

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ReplyViaGoogleMessagesSettingsTest : GatewayTestBase() {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun loadingDisablesTheRow() = render(enabled = null, installed = true) {
        compose.onNodeWithTag("reply-via-gm-row").assertIsNotEnabled()
        compose.onNodeWithTag("reply-via-gm-row-switch", useUnmergedTree = true).fetchSemanticsNode()
    }

    @Test fun missingGoogleMessagesDisablesTheOffRow() = render(enabled = false, installed = false) {
        compose.onNodeWithTag("reply-via-gm-row").assertIsNotEnabled()
        compose.onNodeWithText("Google Messages isn't installed on this phone.").assertExists()
    }

    @Test fun offOpensConsentWithoutChangingTheSwitch() = render(enabled = false, installed = true) {
        compose.onNodeWithTag("reply-via-gm-row").performClick()
        compose.onNodeWithTag("reply-via-gm-consent-sheet").fetchSemanticsNode()
        compose.onNodeWithTag("reply-via-gm-row").assertIsOff()
        compose.runOnIdle { assertEquals(null, applied) }
    }

    @Test fun agreeWithAccessAppliesOnlyAfterConsent() = render(enabled = false, installed = true) {
        compose.onNodeWithTag("reply-via-gm-row").performClick()
        compose.runOnIdle { assertEquals(null, applied) }
        compose.onNodeWithTag("reply-via-gm-consent-sheet-confirm").performClick()
        val transition = replyViaGmConsentTransition(ConsentGatedToggle(ConsentPhase.DISCLOSURE), accessGranted = true)
        assertEquals(ConsentEffect.APPLY, transition.effect)
        assertEquals(ConsentPhase.APPLYING, transition.state.phase)
    }

    @Test fun accessMissingStateRequestsAccessWithoutApplying() = render(enabled = false, installed = true, access = false) {
        compose.onNodeWithTag("reply-via-gm-row").assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "Off, needs notification access"))
        compose.onNodeWithTag("reply-via-gm-row").performClick()
        compose.onNodeWithTag("reply-via-gm-consent-sheet-next-step").fetchSemanticsNode()
        compose.onNodeWithTag("reply-via-gm-consent-sheet-confirm").performClick()
        val transition = replyViaGmConsentTransition(ConsentGatedToggle(ConsentPhase.DISCLOSURE), accessGranted = false)
        assertEquals(ConsentEffect.REQUEST_ACCESS, transition.effect)
        assertEquals(ConsentPhase.AWAITING_ACCESS, transition.state.phase)
        compose.runOnIdle { assertEquals(null, applied) }
    }

    @Test fun pausedOnStateRemainsOperable() = render(enabled = true, installed = false, access = false) {
        compose.onNodeWithTag("reply-via-gm-row").assertIsOn()
        compose.onNodeWithText("Paused: notification access is needed. Replies go by carrier SMS or MMS.").assertExists()
        compose.onNodeWithTag("reply-via-gm-row").performClick()
        compose.runOnIdle { assertEquals(false, applied) }
    }

    @Test fun replyPreferenceIsIndependentOfMirroringPolicy() = runBlocking {
        enrollAndUnlock()
        val preferences = ReplyViaGmPreferences(context)
        val policy = GatewayPolicyHost(context)

        assertFalse(preferences.enabled())
        policy.mirroringEnabled = true
        preferences.setEnabled(true)
        assertTrue(preferences.enabled())
        assertTrue(policy.mirroringEnabled)
        policy.mirroringEnabled = false
        assertTrue(preferences.enabled())
    }

    @Test fun disclosureUsesRequiredTagsCopyAndSafeTermsAtLargeFontScale() = render(
        enabled = false,
        installed = true,
        consent = ConsentGatedToggle(ConsentPhase.DISCLOSURE),
        fontScale = 2f,
    ) {
        compose.onNodeWithTag("reply-via-gm-consent-sheet-title").fetchSemanticsNode()
        compose.onNodeWithTag("reply-via-gm-disclosure").fetchSemanticsNode()
        compose.onNodeWithTag("reply-via-gm-consent-sheet-confirm").assertIsDisplayed()
        compose.onNodeWithTag("reply-via-gm-consent-sheet-dismiss").assertIsDisplayed()
        compose.onNodeWithText("When you reply from your computer or another device", substring = true).fetchSemanticsNode()
        replyCopy().forEach { copy ->
            assertFalse(copy.contains("rcs", ignoreCase = true))
            assertFalse(copy.contains("encrypt", ignoreCase = true))
        }
    }

    @Test fun sectionUsesAHeading() = render(enabled = false, installed = true) {
        val headings = compose.onAllNodes(hasText("Reply through Google Messages"), useUnmergedTree = true)
            .fetchSemanticsNodes().filter { it.config.contains(SemanticsProperties.Heading) }
        assertTrue(headings.isNotEmpty())
    }

    private var applied: Boolean? = null

    private fun render(
        enabled: Boolean?,
        installed: Boolean?,
        access: Boolean = true,
        consent: ConsentGatedToggle = ConsentGatedToggle(),
        fontScale: Float = 1f,
        verify: () -> Unit,
    ) {
        applied = null
        compose.setContent {
            val currentConsent = androidx.compose.runtime.remember { mutableStateOf(consent) }
            CompositionLocalProvider(LocalDensity provides Density(1f, fontScale)) {
                MaterialTheme {
                    ReplyViaGoogleMessagesSettings(
                        enabled = enabled,
                        accessGranted = access,
                        googleMessagesInstalled = installed,
                        consent = currentConsent.value,
                        onConsentChanged = { currentConsent.value = it },
                        onApplyEnabled = { applied = it },
                        onRequestAccess = {},
                    )
                }
            }
        }
        verify()
    }

    private fun replyCopy() = listOf(
        R.string.peppy_toggle_android_reply_via_gm_label,
        R.string.peppy_toggle_android_reply_via_gm_hint_off,
        R.string.peppy_toggle_android_reply_via_gm_hint_on,
        R.string.peppy_toggle_android_reply_via_gm_access_hint,
        R.string.peppy_toggle_android_reply_via_gm_paused_access_hint,
        R.string.peppy_toggle_android_reply_via_gm_paused_missing_hint,
        R.string.peppy_toggle_android_reply_via_gm_not_installed_hint,
        R.string.peppy_toggle_android_reply_via_gm_consent_title,
        R.string.peppy_reply_via_gm_consent_p_how,
        R.string.peppy_reply_via_gm_consent_p_scope,
        R.string.peppy_reply_via_gm_consent_p_unknown,
        R.string.peppy_reply_via_gm_consent_p_read,
        R.string.peppy_reply_via_gm_consent_p_access,
        R.string.peppy_reply_via_gm_consent_p_optin,
        R.string.peppy_help_android_reply_via_gm,
    ).map(context::getString)
}
