package dev.peppy.mobile.enrollment

import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class HostedEnrollmentScreenLayoutTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    // Real system-bar and cutout insets stay on device; the other methods run in the JVM suite.
    @Test
    fun defaultSafeAreaInsetsKeepContentClearOfSystemBarsAndCutout() {
        composeRule.runOnUiThread { composeRule.activity.enableEdgeToEdge() }
        composeRule.setContent {
            HostedEnrollmentScreenLayout(
                centered = true,
                contentTag = "hosted-enrollment-default-content",
            ) {
                Button(modifier = Modifier.testTag("hosted-layout-default-action"), onClick = {}) {
                    Text("Continue with Google")
                }
            }
        }
        composeRule.waitForIdle()

        val insets = currentSystemBarAndCutoutInsets()
        val window = composeRule.onRoot().fetchSemanticsNode().boundsInWindow
        val content = composeRule.onNodeWithTag("hosted-enrollment-default-content")
            .fetchSemanticsNode().boundsInWindow
        val action = composeRule.onNodeWithTag("hosted-layout-default-action")
            .fetchSemanticsNode().boundsInWindow

        for ((label, bounds) in listOf("Content" to content, "Action" to action)) {
            assertTrue("$label top ${bounds.top} must clear inset ${insets.top}", bounds.top >= insets.top)
            assertTrue("$label left ${bounds.left} must clear inset ${insets.left}", bounds.left >= insets.left)
            assertTrue(
                "$label right ${bounds.right} must clear inset ${insets.right}",
                bounds.right <= window.right - insets.right,
            )
            assertTrue(
                "$label bottom ${bounds.bottom} must clear inset ${insets.bottom}",
                bounds.bottom <= window.bottom - insets.bottom,
            )
        }
    }

    private fun currentSystemBarAndCutoutInsets(): Insets = composeRule.runOnUiThread {
        val rootInsets = checkNotNull(ViewCompat.getRootWindowInsets(composeRule.activity.window.decorView))
        rootInsets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
    }
}
