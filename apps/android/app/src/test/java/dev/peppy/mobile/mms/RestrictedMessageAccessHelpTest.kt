package dev.peppy.mobile.mms

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], qualifiers = "w480dp-h800dp")
class RestrictedMessageAccessHelpTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun `help is modal and unsupported access includes unsupported guidance`() {
        val genericHelp = string(R.string.peppy_mms_restricted_help)
        val unsupportedHelp = string(R.string.peppy_mms_restricted_unsupported_help)
        setContent(RestrictedMessageAccess.UNSUPPORTED)

        assertTrue(compose.onAllNodesWithText(genericHelp).fetchSemanticsNodes().isEmpty())
        assertTrue(compose.onAllNodesWithText(unsupportedHelp).fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("mms-restricted-message-access").assertIsDisplayed()
        compose.onNodeWithText(status(RestrictedMessageAccess.UNSUPPORTED)).assertIsDisplayed()

        compose.onNodeWithTag("info-mms-restricted-message-access").performClick()
        compose.onNodeWithTag("info-mms-restricted-message-access-dialog").assertIsDisplayed()
        compose.onNodeWithText("$genericHelp\n\n$unsupportedHelp").assertIsDisplayed()
        compose.onNodeWithTag("info-mms-restricted-message-access-dismiss").performClick()
        compose.onNodeWithTag("info-mms-restricted-message-access-dialog").assertDoesNotExist()
        compose.onNodeWithText(status(RestrictedMessageAccess.UNSUPPORTED)).assertIsDisplayed()
    }

    @Test fun `allowed access keeps only generic help in modal`() = assertGenericHelpOnly(RestrictedMessageAccess.ALLOWED)

    @Test fun `not allowed access keeps only generic help in modal`() = assertGenericHelpOnly(RestrictedMessageAccess.NOT_ALLOWED)

    @Test fun `unknown access keeps only generic help in modal`() = assertGenericHelpOnly(RestrictedMessageAccess.UNKNOWN)

    private fun assertGenericHelpOnly(access: RestrictedMessageAccess) {
        val genericHelp = string(R.string.peppy_mms_restricted_help)
        val unsupportedHelp = string(R.string.peppy_mms_restricted_unsupported_help)
        setContent(access)
        compose.onNodeWithTag("mms-restricted-message-access").assertIsDisplayed()
        compose.onNodeWithText(status(access)).assertIsDisplayed()
        compose.onNodeWithTag("info-mms-restricted-message-access").performClick()
        compose.onNodeWithText(genericHelp).assertIsDisplayed()
        assertTrue(compose.onAllNodesWithText(unsupportedHelp).fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("info-mms-restricted-message-access-dismiss").performClick()
    }

    @Test fun `large text narrow layout keeps help and status in bounds`() {
        setContent(RestrictedMessageAccess.NOT_ALLOWED, width = 344.dp, fontScale = 1.3f)

        val container = compose.onNodeWithTag("mms-restricted-message-access").fetchSemanticsNode().boundsInRoot
        val status = compose.onNodeWithText(status(RestrictedMessageAccess.NOT_ALLOWED)).fetchSemanticsNode().boundsInRoot
        val help = compose.onNodeWithTag("info-mms-restricted-message-access").fetchSemanticsNode().boundsInRoot
        assertTrue(status.left >= container.left && status.right <= container.right)
        assertTrue(help.left >= container.left && help.right <= container.right)
        compose.onNodeWithTag("info-mms-restricted-message-access").performClick()
        compose.onNodeWithText(string(R.string.peppy_mms_restricted_help)).assertIsDisplayed()
    }

    private fun setContent(access: RestrictedMessageAccess, width: androidx.compose.ui.unit.Dp = 400.dp, fontScale: Float = 1f) {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                MaterialTheme { Box(Modifier.width(width)) { RestrictedMessageAccessStatus(access) } }
            }
        }
    }

    private fun string(id: Int): String = ApplicationProvider.getApplicationContext<android.content.Context>().getString(id)

    private fun status(access: RestrictedMessageAccess): String = string(when (access) {
        RestrictedMessageAccess.ALLOWED -> R.string.peppy_mms_restricted_status_allowed
        RestrictedMessageAccess.NOT_ALLOWED -> R.string.peppy_mms_restricted_status_not_allowed
        RestrictedMessageAccess.UNSUPPORTED -> R.string.peppy_mms_restricted_status_unsupported
        RestrictedMessageAccess.UNKNOWN -> R.string.peppy_mms_restricted_status_unknown
    })
}
