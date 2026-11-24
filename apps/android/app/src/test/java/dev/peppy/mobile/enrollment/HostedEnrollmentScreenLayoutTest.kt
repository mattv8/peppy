package dev.peppy.mobile.enrollment

import android.content.Context
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.*
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], qualifiers = "w411dp-h891dp-xxhdpi")
class HostedEnrollmentScreenLayoutTest {
    private val composeRule = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(composeRule)

    @Test
    fun startContentStaysBelowRepresentativeSafeAreaInsets() {
        composeRule.setContent {
            HostedEnrollmentScreenLayout(
                centered = true,
                safeAreaInsets = representativeSafeAreaInsets(),
                contentTag = "hosted-enrollment-start-content",
            ) {
                Button(modifier = Modifier.testTag("hosted-layout-start-action"), onClick = {}) {
                    Text("Continue with Google")
                }
            }
        }

        val contentTop = composeRule.onNodeWithTag("hosted-layout-start-action")
            .fetchSemanticsNode().boundsInRoot.top
        val rootTop = composeRule.onRoot().fetchSemanticsNode().boundsInRoot.top
        val minimumTop = rootTop + representativeTopInsetPx() + contentPaddingPx()

        assertTrue("Start content top $contentTop must clear safe area $minimumTop", contentTop >= minimumTop)
    }

    @Test
    fun startContentIsCenteredInSafeViewport() {
        composeRule.setContent {
            HostedEnrollmentScreenLayout(
                centered = true,
                safeAreaInsets = representativeSafeAreaInsets(),
                contentTag = "hosted-enrollment-start-content",
            ) {
                Button(modifier = Modifier.testTag("hosted-layout-start-action"), onClick = {}) {
                    Text("Continue with Google")
                }
            }
        }

        val bounds = composeRule.onNodeWithTag("hosted-layout-start-action")
            .fetchSemanticsNode().boundsInRoot
        val rootBounds = composeRule.onRoot().fetchSemanticsNode().boundsInRoot
        val safeAreaCenter = (rootBounds.top + representativeTopInsetPx() +
            rootBounds.bottom - representativeBottomInsetPx()) / 2f
        val contentCenter = (bounds.top + bounds.bottom) / 2f
        val tolerance = 2f * ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics.density

        assertTrue(
            "Start content center $contentCenter must match safe viewport center $safeAreaCenter",
            kotlin.math.abs(contentCenter - safeAreaCenter) <= tolerance,
        )
    }

    @Test
    fun topAlignedContentStartsAfterSafeAreaAndContentPadding() {
        composeRule.setContent {
            HostedEnrollmentScreenLayout(
                centered = false,
                safeAreaInsets = representativeSafeAreaInsets(),
                contentTag = "hosted-enrollment-top-aligned-content",
            ) {
                Box(Modifier.fillMaxWidth().height(1.dp).testTag("hosted-layout-top-marker"))
                Button(modifier = Modifier.testTag("hosted-layout-top-aligned-action"), onClick = {}) {
                    Text("Continue")
                }
            }
        }

        val actionTop = composeRule.onNodeWithTag("hosted-layout-top-marker")
            .fetchSemanticsNode().boundsInRoot.top
        val rootTop = composeRule.onRoot().fetchSemanticsNode().boundsInRoot.top
        val expectedTop = rootTop + representativeTopInsetPx() + contentPaddingPx()
        val tolerance = 2f * ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics.density

        assertTrue(
            "Top-aligned content top $actionTop must equal $expectedTop",
            kotlin.math.abs(actionTop - expectedTop) <= tolerance,
        )
    }

    @Test
    fun tallFormContentScrollsToLastActionInImeReducedLargeFontViewport() {
        composeRule.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale = 1.3f)) {
                Box(Modifier.fillMaxWidth().height(914.dp)) {
                    HostedEnrollmentScreenLayout(
                        centered = false,
                        safeAreaInsets = representativeImeSafeAreaInsets(),
                    ) {
                        repeat(24) { Text("A long form field label that scales with accessibility text") }
                        Button(modifier = Modifier.testTag("hosted-layout-last-action"), onClick = {}) {
                            Text("Continue")
                        }
                    }
                }
            }
        }

        val lastAction = composeRule.onNodeWithTag("hosted-layout-last-action")
        lastAction.assertIsNotDisplayed()
        lastAction.performScrollTo()
        lastAction.assertIsDisplayed()
    }

    @Test
    fun tallCenteredContentScrollsToLastAction() {
        composeRule.setContent {
            Box(Modifier.fillMaxWidth().height(475.dp)) {
                HostedEnrollmentScreenLayout(
                    centered = true,
                    safeAreaInsets = representativeSafeAreaInsets(),
                ) {
                    repeat(24) { Text("Centered content can still overflow on a compact viewport") }
                    Button(modifier = Modifier.testTag("hosted-layout-centered-last-action"), onClick = {}) {
                        Text("Continue")
                    }
                }
            }
        }

        val lastAction = composeRule.onNodeWithTag("hosted-layout-centered-last-action")
        lastAction.assertIsNotDisplayed()
        lastAction.performScrollTo()
        lastAction.assertIsDisplayed()
    }

    private fun representativeSafeAreaInsets() = WindowInsets(
        top = representativeTopInsetPx().toInt(),
        bottom = representativeBottomInsetPx().toInt(),
    )

    private fun representativeImeSafeAreaInsets() = WindowInsets(
        top = representativeTopInsetPx().toInt(),
        bottom = with(ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics) { 363f * density }.toInt(),
    )

    private fun representativeTopInsetPx() = with(ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics) { 51.8f * density }

    private fun representativeBottomInsetPx() = with(ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics) { 24f * density }

    private fun contentPaddingPx() = with(ApplicationProvider.getApplicationContext<Context>().resources.displayMetrics) { 24f * density }
}
