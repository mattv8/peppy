package dev.peppy.mobile.enrollment

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import dev.peppy.mobile.security.SecureWindow
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class EnrollmentSetupTokenSecureWindowTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    private class RecordingSecureWindow : SecureWindow {
        val calls = mutableListOf<Boolean>()
        override fun setSecure(enabled: Boolean) {
            calls += enabled
        }
    }

    @Test fun secureWindowFollowsTokenVisibility() {
        val window = RecordingSecureWindow()
        var visible by mutableStateOf(false)
        compose.setContent { SetupTokenSecureWindow(visible, window) }

        compose.runOnIdle { assertEquals(emptyList<Boolean>(), window.calls) }
        compose.runOnIdle { visible = true }
        compose.runOnIdle { assertEquals(listOf(true), window.calls) }
        compose.runOnIdle { visible = false }
        compose.runOnIdle { assertEquals(listOf(true, false), window.calls) }
    }

    @Test fun leavingCompositionWhileVisibleClearsSecureWindow() {
        val window = RecordingSecureWindow()
        var shown by mutableStateOf(true)
        compose.setContent { if (shown) SetupTokenSecureWindow(true, window) }

        compose.runOnIdle { assertEquals(listOf(true), window.calls) }
        compose.runOnIdle { shown = false }
        compose.runOnIdle { assertEquals(listOf(true, false), window.calls) }
    }
}
