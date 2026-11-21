package dev.peppy.mobile.mms

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RestrictedAccessActionReleaseTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test
    fun `restricted access is hidden on release`() {
        assertFalse(restrictedMessageAccessVisible())
        compose.setContent {
            MaterialTheme {
                MmsSettings(context = ApplicationProvider.getApplicationContext(), refresh = 0, databaseOpen = false, onChanged = {}, detailsOnly = true)
            }
        }
        compose.onAllNodes(SemanticsMatcher("restricted access tag") {
            it.config.getOrNull(SemanticsProperties.TestTag)?.startsWith("mms-restricted-") == true
        }, useUnmergedTree = true).assertCountEquals(0)
        compose.onNodeWithText("Restricted message access").assertDoesNotExist()
    }
}
