package dev.peppy.mobile.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.test.click
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.Dp
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

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class DetailsPageComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun backButtonCallsOnBack() {
        var backs = 0
        compose.setContent { MaterialTheme { DetailsPageHeader("Details", "x", onBack = { backs++ }) } }
        compose.onNodeWithTag("x-back").performClick()
        compose.runOnIdle { assertEquals(1, backs) }
    }

    @Test fun backButtonHasBackContentDescription() {
        compose.setContent { MaterialTheme { DetailsPageHeader("Details", "x", onBack = {}) } }
        compose.onNodeWithTag("x-back").assert(hasContentDescription("Back"))
    }

    @Test fun backButtonHasMinimumTouchTarget() {
        compose.setContent { MaterialTheme { DetailsPageHeader("Details", "x", onBack = {}) } }
        compose.onNodeWithTag("x-back")
            .assertWidthIsAtLeast(48.dp)
            .assertHeightIsAtLeast(48.dp)
    }

    @Test fun titleHasHeadingSemantics() {
        compose.setContent { MaterialTheme { DetailsPageHeader("Details", "x", onBack = {}) } }
        compose.onNodeWithTag("x-title").assert(SemanticsMatcher.keyIsDefined(SemanticsProperties.Heading))
    }

    @Test @Config(qualifiers = "w480dp-h800dp")
    fun statusRowKeepsValueInsideNormalWideRow() {
        assertStatusRowLayout(400.dp, 1f, stacked = false)
    }

    @Test fun statusRowKeepsValueInsideNormalNarrowRow() {
        assertStatusRowLayout(280.dp, 1f, stacked = true)
    }

    @Test @Config(qualifiers = "w480dp-h800dp")
    fun statusRowStacksAtLargeFontEvenWhenWide() {
        assertStatusRowLayout(400.dp, 1.5f, stacked = true)
    }

    @Test fun statusRowKeepsValueInsideLargeNarrowRow() {
        assertStatusRowLayout(280.dp, 1.5f, stacked = true)
    }

    private fun assertStatusRowLayout(width: Dp, fontScale: Float, stacked: Boolean) {
        val label = "Contacts on this phone"
        val value = "Waiting for permission to read contacts on this phone"
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                MaterialTheme {
                    Box(Modifier.width(width)) { DetailsStatusRow("status", label, value) }
                }
            }
        }
        val rowBounds = compose.onNodeWithTag("status").fetchSemanticsNode().boundsInRoot
        val labelBounds = compose.onNodeWithText(label).fetchSemanticsNode().boundsInRoot
        val valueBounds = compose.onNodeWithText(value).fetchSemanticsNode().boundsInRoot
        assertTrue(valueBounds.width > 0f)
        assertTrue(valueBounds.height > 0f)
        assertTrue(valueBounds.left >= rowBounds.left)
        assertTrue(valueBounds.right <= rowBounds.right)
        assertTrue(valueBounds.top >= rowBounds.top)
        assertTrue(valueBounds.bottom <= rowBounds.bottom)
        if (stacked) {
            assertTrue(valueBounds.top > labelBounds.bottom)
        } else {
            assertEquals(labelBounds.top, valueBounds.top, 0.5f)
            assertTrue(valueBounds.left > labelBounds.right)
        }
    }

    @Test fun navigationRowWithoutAttentionHasNoBadge() {
        compose.setContent { MaterialTheme { DetailsNavigationRow("row", "Sync details", null, onClick = {}) } }
        compose.onNodeWithTag("row").fetchSemanticsNode()
        assertTrue(compose.onAllNodesWithTag("row-attention", useUnmergedTree = true).fetchSemanticsNodes().isEmpty())
    }

    @Test fun navigationRowWithAttentionShowsBadgeAndClicks() {
        var clicks = 0
        compose.setContent { MaterialTheme { DetailsNavigationRow("row", "Sync details", "Needs attention", onClick = { clicks++ }) } }
        compose.onNodeWithTag("row-attention", useUnmergedTree = true).fetchSemanticsNode()
        compose.onNodeWithText("Needs attention", useUnmergedTree = true).fetchSemanticsNode()
        compose.onNodeWithTag("row").performClick()
        compose.runOnIdle { assertEquals(1, clicks) }
    }

    @Test fun settingToggleRowTrailingActionDoesNotToggle() {
        var trailClicked = false
        var toggled = false
        compose.setContent {
            MaterialTheme {
                SettingToggleRow(
                    title = "Wait for Wi-Fi",
                    checked = false,
                    enabled = true,
                    onCheckedChange = { toggled = true },
                    tag = "wifi",
                    titleTrailing = {
                        IconButton(onClick = { trailClicked = true }, modifier = Modifier.testTag("trail")) { Text("i") }
                    },
                )
            }
        }
        compose.onNodeWithTag("trail", useUnmergedTree = true).performClick()
        compose.runOnIdle {
            assertTrue(trailClicked)
            assertFalse(toggled)
        }
    }

    @Test fun settingToggleRowTrailingTouchDoesNotToggle() {
        var trailClicked = false
        var toggled = false
        compose.setContent {
            MaterialTheme {
                SettingToggleRow(
                    title = "Wait for Wi-Fi",
                    checked = false,
                    enabled = true,
                    onCheckedChange = { toggled = true },
                    tag = "wifi",
                    titleTrailing = {
                        IconButton(onClick = { trailClicked = true }, modifier = Modifier.testTag("trail")) { Text("i") }
                    },
                )
            }
        }
        compose.onNodeWithTag("trail", useUnmergedTree = true).performTouchInput { click() }
        compose.runOnIdle {
            assertTrue(trailClicked)
            assertFalse(toggled)
        }
    }
}
