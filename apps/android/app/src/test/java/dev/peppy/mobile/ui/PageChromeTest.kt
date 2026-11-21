package dev.peppy.mobile.ui

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PageChromeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun detailPageRegistersChromeAndClearsItWhenLeaving() {
        val host = PageChromeHost()
        val shown = mutableStateOf(true)
        compose.setContent {
            MaterialTheme {
                CompositionLocalProvider(LocalPageChromeHost provides host) {
                    if (shown.value) DetailPage(PageChrome("Devices", "devices", onBack = {})) { Text("body") }
                }
            }
        }
        compose.runOnIdle {
            assertEquals("Devices", host.current?.title)
            assertEquals("devices", host.current?.tag)
        }
        compose.runOnIdle { shown.value = false }
        compose.runOnIdle { assertNull(host.current) }
    }

    @Test fun swappingDetailPagesLeavesTheNewPageInTheHost() {
        val host = PageChromeHost()
        val page = mutableStateOf("a")
        compose.setContent {
            MaterialTheme {
                CompositionLocalProvider(LocalPageChromeHost provides host) {
                    if (page.value == "a") DetailPage(PageChrome("Page A", "a", onBack = {})) {}
                    else DetailPage(PageChrome("Page B", "b", onBack = {})) {}
                }
            }
        }
        compose.runOnIdle { page.value = "b" }
        compose.runOnIdle { assertEquals("b", host.current?.tag) }
    }

    @Test fun clearOnlyRemovesTheMatchingTag() {
        val host = PageChromeHost()
        host.show(PageChrome("Devices", "devices", onBack = {}))
        host.clear("other")
        assertEquals("devices", host.current?.tag)
        host.clear("devices")
        assertNull(host.current)
    }

    @Test fun recreatedActionListKeepsOneRegisteredChromeAndRunsTheLatestAction() {
        val host = PageChromeHost()
        val counter = mutableStateOf(0)
        var lastSeen = -1
        compose.setContent {
            MaterialTheme {
                CompositionLocalProvider(LocalPageChromeHost provides host) {
                    val count = counter.value
                    DetailPage(
                        PageChrome(
                            "Devices",
                            "devices",
                            onBack = {},
                            actions = listOf(PageAction(Icons.Default.Info, "Refresh", "devices-refresh", onClick = { lastSeen = count })),
                        ),
                    ) { Text("count $count") }
                }
            }
        }
        val first = host.current
        assertEquals("Devices", first?.title)
        repeat(5) {
            compose.runOnIdle { counter.value = counter.value + 1 }
            compose.runOnIdle { assertSame(first, host.current) }
        }
        compose.runOnIdle { host.current?.actions?.single()?.onClick?.invoke() }
        compose.runOnIdle { assertEquals(5, lastSeen) }
    }

    @Test fun withoutHostTheFallbackHeaderBackAndActionsRender() {
        var backs = 0
        var refreshes = 0
        compose.setContent {
            MaterialTheme {
                DetailPage(
                    PageChrome(
                        "Fallback",
                        "fb",
                        onBack = { backs++ },
                        actions = listOf(PageAction(Icons.Default.Info, "Refresh", "fb-refresh", onClick = { refreshes++ })),
                    ),
                ) { Text("body") }
            }
        }
        compose.onNodeWithTag("fb-header").fetchSemanticsNode()
        compose.onNodeWithTag("fb-back").performClick()
        compose.onNodeWithTag("fb-refresh").performClick()
        compose.runOnIdle {
            assertEquals(1, backs)
            assertEquals(1, refreshes)
        }
    }

    @Test fun topBarShowsHostChromeTitleBackAndActions() {
        var backs = 0
        var refreshes = 0
        compose.setContent {
            MaterialTheme {
                val host = remember {
                    PageChromeHost().apply {
                        show(
                            PageChrome(
                                "Devices",
                                "devices",
                                onBack = { backs++ },
                                actions = listOf(PageAction(Icons.Default.Info, "Refresh", "devices-refresh", onClick = { refreshes++ })),
                            ),
                        )
                    }
                }
                PageChromeTopAppBar(host, fallbackTitle = "Account")
            }
        }
        compose.onNodeWithText("Devices").fetchSemanticsNode()
        compose.onNodeWithTag("devices-back").performClick()
        compose.onNodeWithTag("devices-refresh").performClick()
        compose.runOnIdle {
            assertEquals(1, backs)
            assertEquals(1, refreshes)
        }
    }

    @Test fun topBarShowsFallbackTitleWithoutChrome() {
        compose.setContent { MaterialTheme { PageChromeTopAppBar(PageChromeHost(), fallbackTitle = "Account") } }
        compose.onNodeWithText("Account").fetchSemanticsNode()
    }
}
