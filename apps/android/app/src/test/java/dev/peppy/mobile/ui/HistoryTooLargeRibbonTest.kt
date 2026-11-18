package dev.peppy.mobile.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.account.RibbonSlot
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
import uniffi.peppy_mobile_bindings.NativeNoticeSeverity

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class HistoryTooLargeRibbonTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private val context = ApplicationProvider.getApplicationContext<android.content.Context>()

    @Test
    fun modelIsAWarningWithoutDismissAndOpensSyncDetails() {
        val model = buildSystemRibbonModel(context, RibbonSlot.HistoryTooLarge, nowMs = 0L)

        assertEquals("history_too_large", model.key)
        assertEquals(NativeNoticeSeverity.WARNING, model.severity)
        assertEquals("History too large for this version", model.title)
        assertEquals("Update Peppy to continue. Nothing is lost.", model.body)
        assertEquals("Details", model.actionLabel)
        assertEquals("Opens sync details", model.actionDescription)
        assertFalse(model.dismissible)
        assertTrue(model.manageUrl == null)
    }

    @Test
    fun ribbonShowsTagsAndActionCallsSyncDetails() {
        var opened = 0
        compose.setContent {
            MaterialTheme {
                SystemRibbon(
                    model = buildSystemRibbonModel(context, RibbonSlot.HistoryTooLarge, nowMs = 0L),
                    linkFailed = false,
                    onAction = { opened++ },
                    onDismiss = {},
                )
            }
        }

        compose.onNodeWithTag("system-ribbon").fetchSemanticsNode()
        compose.onNodeWithTag("system-ribbon-title").assertTextEquals("History too large for this version")
        assertTrue(compose.onAllNodesWithTag("system-ribbon-dismiss").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("system-ribbon-action").performClick()
        assertEquals(1, opened)
    }
}
