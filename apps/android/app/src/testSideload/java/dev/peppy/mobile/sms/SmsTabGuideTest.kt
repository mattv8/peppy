package dev.peppy.mobile.sms

import android.content.ActivityNotFoundException
import android.content.ContextWrapper
import android.content.Intent
import android.os.Bundle
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.theme.PeppyTheme
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private class SmsGuideContext(base: android.content.Context) : ContextWrapper(base) {
    var rejectActivityLaunch = true
    var startedIntent: Intent? = null

    override fun startActivity(intent: Intent) {
        if (rejectActivityLaunch) throw ActivityNotFoundException()
        startedIntent = intent
    }

    override fun startActivity(intent: Intent, options: Bundle?) = startActivity(intent)
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], qualifiers = "w412dp-h1600dp")
class SmsTabGuideTest {
    private val compose = createComposeRule(Dispatchers.Main)
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun `Sync details guide opens its exact URL and keeps failures separate from SMS action errors`() {
        val context = SmsGuideContext(ApplicationProvider.getApplicationContext())
        compose.setContent {
            PeppyTheme { SmsTab(context, refresh = 0, databaseOpen = false, onChanged = {}) }
        }
        compose.waitUntil { compose.onAllNodesWithTag("sms-details-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("sms-details-row").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("sms-details-guide").fetchSemanticsNodes().isNotEmpty() }

        compose.onNodeWithTag("sms-details-guide").performClick()

        compose.onNodeWithTag("sms-details-guide-error")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite))
        check(compose.onAllNodesWithTag("sms-action-error").fetchSemanticsNodes().isEmpty())
        context.rejectActivityLaunch = false
        compose.onNodeWithTag("sms-details-guide").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("sms-details-guide-error").fetchSemanticsNodes().isEmpty() }
        val intent = requireNotNull(context.startedIntent)
        assertEquals(Intent.ACTION_VIEW, intent.action)
        assertEquals("https://peppy.pro/docs/android", intent.data.toString())
    }
}
