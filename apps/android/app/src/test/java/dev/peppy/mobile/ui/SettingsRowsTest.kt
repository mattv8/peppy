package dev.peppy.mobile.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.click
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
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
class SettingsRowsTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private val badges = listOf(
        BadgeSpec("This device", "badge-device"),
        BadgeSpec("Phone gateway", "badge-gateway"),
        BadgeSpec("Last seen 2h ago", "badge-seen"),
        BadgeSpec("Sending paused", "badge-paused", BadgeTone.WARNING),
    )

    @Test fun navigationRowInvokesCallbackAndShowsSummary() {
        var clicks = 0
        compose.setContent { MaterialTheme { NavigationRow("nav", "Devices", onClick = { clicks++ }, summary = "4 active") } }
        compose.onNodeWithText("4 active", useUnmergedTree = true).fetchSemanticsNode()
        compose.onNodeWithTag("nav").performClick()
        compose.runOnIdle { assertEquals(1, clicks) }
    }

    @Test fun navigationRowWithInfoPlacesInfoRightAfterLabel() {
        compose.setContent {
            MaterialTheme {
                Box(Modifier.width(400.dp).testTag("row-host")) {
                    NavigationRow("nav", "Privacy Policy", onClick = {}, info = InfoSpec("Privacy Policy", "Scope note.", "nav-info"))
                }
            }
        }
        val gap = with(compose.density) { 8.dp.toPx() }
        val label = compose.onNodeWithText("Privacy Policy", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val info = compose.onNodeWithTag("nav-info", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        assertTrue(info.left >= label.right)
        assertTrue(info.left - label.right <= gap)
    }

    @Test fun navigationRowWithInfoKeepsInfoAndChevronVisibleForLongLabel() = assertLongLabelKeepsControlsInRow(1f)

    @Test fun navigationRowWithInfoKeepsInfoAndChevronVisibleForLongLabelAtLargeFont() = assertLongLabelKeepsControlsInRow(2f)

    private fun assertLongLabelKeepsControlsInRow(fontScale: Float) {
        val longLabel = "Open-source licenses ".repeat(30)
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                MaterialTheme {
                    Box(Modifier.width(400.dp).testTag("row-host")) {
                        NavigationRow("nav", longLabel, onClick = {}, info = InfoSpec("Licenses", "Scope note.", "nav-info"))
                    }
                }
            }
        }
        val row = compose.onNodeWithTag("row-host").fetchSemanticsNode().boundsInRoot
        val label = compose.onNodeWithText(longLabel, useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val info = compose.onNodeWithTag("nav-info", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val chevron = compose.onNodeWithTag("nav-trailing", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val message = "fontScale=$fontScale row=$row label=$label info=$info chevron=$chevron"
        assertTrue(message, info.width > 0f && info.height > 0f)
        assertTrue(message, chevron.width > 0f && chevron.height > 0f)
        assertInside(info, row, message)
        assertInside(chevron, row, message)
        assertTrue(message, label.width > row.width / 2f)
        assertTrue(message, info.left >= label.right)
    }

    private fun assertInside(inner: Rect, outer: Rect, message: String) {
        assertTrue(message, inner.left >= outer.left && inner.right <= outer.right)
        assertTrue(message, inner.top >= outer.top && inner.bottom <= outer.bottom)
    }

    @Test fun navigationRowWithInfoClickingTrailingAreaInvokesOnClickOnceAndRowHasOneClickAction() {
        var clicks = 0
        compose.setContent {
            MaterialTheme {
                Box(Modifier.width(400.dp).testTag("row-host")) {
                    NavigationRow("nav", "Privacy Policy", onClick = { clicks++ }, info = InfoSpec("Privacy Policy", "Scope note.", "nav-info"))
                }
            }
        }
        assertTrue(compose.onNodeWithTag("nav").fetchSemanticsNode().config.contains(SemanticsActions.OnClick))
        compose.onNodeWithTag("row-host").performTouchInput { click(Offset(width - 4f, height / 2f)) }
        compose.runOnIdle { assertEquals(1, clicks) }
    }

    @Test fun navigationRowWithInfoKeepsInfoButtonOutOfTheRowClick() {
        compose.setContent {
            MaterialTheme {
                NavigationRow("nav", "Privacy Policy", onClick = {}, info = InfoSpec("Privacy Policy", "Scope note.", "nav-info"))
            }
        }
        compose.onNodeWithTag("nav-info").assertHasClickAction()
    }

    @Test fun externalLinkRowExposesOpensInBrowserClickLabel() {
        compose.setContent { MaterialTheme { ExternalLinkRow("ext", "Privacy Policy", onClick = {}) } }
        val expected = ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(R.string.peppy_about_opens_in_browser)
        val label = compose.onNodeWithTag("ext").fetchSemanticsNode().config[SemanticsActions.OnClick].label
        assertEquals(expected, label)
    }

    @Test fun actionRowButtonHasTagAndEnabledState() {
        compose.setContent {
            MaterialTheme { ActionRow("sync", "Sync", "Retry", onAction = {}, enabled = false) }
        }
        compose.onNodeWithTag("sync-button").assertIsNotEnabled()
    }

    @Test fun actionRowStacksButtonBelowLabelAtLargeFontAndStartsAtLabel() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                MaterialTheme { ActionRow("sync", "Notifications are off", "Allow", onAction = {}) }
            }
        }
        val label = compose.onNodeWithText("Notifications are off", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val button = compose.onNodeWithTag("sync-button").fetchSemanticsNode().boundsInRoot
        assertTrue("label=$label button=$button", button.top > label.bottom)
        assertTrue("label=$label button=$button", kotlin.math.abs(button.left - label.left) < 1f)
    }

    @Test fun actionRowButtonInvokesActionWhenEnabled() {
        var actions = 0
        compose.setContent {
            MaterialTheme { ActionRow("sync", "Sync", "Retry", onAction = { actions++ }) }
        }
        compose.onNodeWithTag("sync-button").assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(1, actions) }
    }

    @Test fun destructiveRowButtonInvokesActionAndUsesRowLabelAsName() {
        var actions = 0
        compose.setContent {
            MaterialTheme {
                DestructiveRow("vault", "Delete vault", "Removes this phone's copy.", "Delete", onAction = { actions++ })
            }
        }
        compose.onNodeWithTag("vault-button").assert(hasContentDescription("Delete vault")).performClick()
        compose.runOnIdle { assertEquals(1, actions) }
    }

    @Test fun destructiveRowAlwaysStacksButtonBelowConsequenceEvenWhenWide() {
        compose.setContent {
            MaterialTheme {
                Box(Modifier.width(600.dp)) {
                    DestructiveRow("vault", "Delete vault", "Removes this phone's copy.", "Delete", onAction = {})
                }
            }
        }
        val consequence = compose.onNodeWithText("Removes this phone's copy.", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val button = compose.onNodeWithTag("vault-button").fetchSemanticsNode().boundsInRoot
        assertTrue(button.top > consequence.bottom)
    }

    @Test fun destructiveRowStacksButtonBelowConsequenceAtLargeFontScale() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                MaterialTheme {
                    DestructiveRow("vault", "Delete vault", "Removes this phone's copy.", "Delete", onAction = {})
                }
            }
        }
        val consequence = compose.onNodeWithText("Removes this phone's copy.", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        val button = compose.onNodeWithTag("vault-button").fetchSemanticsNode().boundsInRoot
        assertTrue(button.top > consequence.bottom)
    }

    @Test fun dangerGroupRendersHeadingAndContent() {
        compose.setContent {
            MaterialTheme {
                SettingsGroup("danger", "Danger zone", danger = true) { Text("Inside danger") }
            }
        }
        compose.onNodeWithText("Danger zone", useUnmergedTree = true)
            .assert(SemanticsMatcher.keyIsDefined(SemanticsProperties.Heading))
        compose.onNodeWithText("Inside danger").fetchSemanticsNode()
    }

    @Test fun attentionCalloutActionInvokes() {
        var actions = 0
        compose.setContent {
            MaterialTheme {
                AttentionCallout("callout", "Storage is almost full.", "Manage", onAction = { actions++ })
            }
        }
        compose.onNodeWithTag("callout-action").performClick()
        compose.runOnIdle { assertEquals(1, actions) }
    }

    @Test fun singleLineBadgesCollapseIntoCountWhenNarrow() {
        compose.setContent {
            MaterialTheme {
                SingleLineBadges(badges, overflowText = { "+$it" }, tag = "badges", modifier = Modifier.width(120.dp))
            }
        }
        val moreText = placedNodes("badges-more").single().children.first().config[SemanticsProperties.Text].single().text
        assertTrue(moreText.matches(Regex("\\+[1-4]")))
        val shown = badges.count { badge -> placed(badge.tag) }
        assertEquals(4 - moreText.drop(1).toInt(), shown)
        assertTrue(shown < badges.size)
        compose.onNodeWithTag("badges").assert(hasContentDescription(badges.joinToString(", ") { it.text }))
    }

    @Test fun singleLineBadgesShowEveryBadgeWhenWide() {
        compose.setContent {
            MaterialTheme {
                SingleLineBadges(badges, overflowText = { "+$it" }, tag = "badges", modifier = Modifier.width(600.dp))
            }
        }
        badges.forEach { assertTrue(placed(it.tag)) }
        assertFalse(placed("badges-more"))
        compose.onNodeWithTag("badges").assert(hasContentDescription(badges.joinToString(", ") { it.text }))
    }

    /**
     * Sweeps the container width: at every width the placed badges are a prefix, the overflow badge
     * (when shown) counts the rest, the container never wraps (height equals the badge height), and
     * the sweep reaches both "two badges + +2" and "overflow only".
     */
    @Test fun singleLineBadgesSweepKeepsOnePrefixLineWithCountAndReachesTwoAndOverflowOnly() {
        val width = mutableStateOf(600.dp)
        compose.setContent {
            MaterialTheme {
                SingleLineBadges(badges, overflowText = { "+$it" }, tag = "badges", modifier = Modifier.width(width.value))
            }
        }
        var reachedTwo = false
        var reachedOverflowOnly = false
        var previousShown = 0
        for (dp in 1..400 step 2) {
            compose.runOnIdle { width.value = dp.dp }
            compose.waitForIdle()
            val shown = badges.count { placed(it.tag) }
            val message = "width=${dp}dp shown=$shown"
            badges.indices.forEach { index -> assertEquals(message, index < shown, placed(badges[index].tag)) }
            assertTrue(message, shown >= previousShown)
            previousShown = shown
            if (shown < badges.size) {
                val more = placedNodes("badges-more").single()
                assertEquals(message, "+${badges.size - shown}", more.children.first().config[SemanticsProperties.Text].single().text)
                val lineHeight = more.layoutInfo.height
                assertEquals(message, lineHeight.toFloat(), containerBounds().height, 0.5f)
            } else {
                assertFalse(message, placed("badges-more"))
            }
            if (shown == 2) reachedTwo = true
            if (shown == 0) reachedOverflowOnly = true
        }
        assertTrue(reachedTwo)
        assertTrue(reachedOverflowOnly)
    }

    @Test fun pageChromeTagsFollowAddedAndRemovedActionsWithoutLoop() {
        val host = PageChromeHost()
        val extra = mutableStateOf(false)
        compose.setContent {
            MaterialTheme {
                CompositionLocalProvider(LocalPageChromeHost provides host) {
                    DetailPage(
                        PageChrome(
                            "Devices",
                            "devices",
                            onBack = {},
                            actions = buildList {
                                add(PageAction(Icons.Default.Info, "Refresh", "devices-refresh", onClick = {}))
                                if (extra.value) add(PageAction(Icons.Default.Info, "Add", "devices-add", onClick = {}))
                            },
                        ),
                    ) {}
                }
            }
        }
        fun tags() = host.current?.actions?.map { it.tag }
        compose.runOnIdle { assertEquals(listOf("devices-refresh"), tags()) }
        repeat(2) {
            compose.runOnIdle { extra.value = true }
            compose.runOnIdle { assertEquals(listOf("devices-refresh", "devices-add"), tags()) }
            compose.runOnIdle { extra.value = false }
            compose.runOnIdle { assertEquals(listOf("devices-refresh"), tags()) }
        }
    }

    private fun placed(tag: String): Boolean = placedNodes(tag).isNotEmpty()

    private fun placedNodes(tag: String) =
        compose.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().filter { it.layoutInfo.isPlaced }

    private fun containerBounds() = compose.onNodeWithTag("badges", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
}
