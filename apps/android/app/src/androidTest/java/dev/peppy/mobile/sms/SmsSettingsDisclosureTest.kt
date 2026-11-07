package dev.peppy.mobile.sms

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.peppy.mobile.R
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith


@RunWith(AndroidJUnit4::class)
class SmsSettingsDisclosureTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun fullDisclosureIsReachableFromTextHistoryInfoAtNormalFont() {
        assertFullDisclosureIsReachable(fontScale = 1f)
    }

    @Test
    fun fullDisclosureIsReachableFromTextHistoryInfoAtLargeFont() {
        assertFullDisclosureIsReachable(fontScale = 1.3f)
    }

    private fun assertFullDisclosureIsReachable(fontScale: Float) {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    SmsSettings(
                        context = LocalContext.current,
                        historyState = SmsHistoryState.WAITING_FOR_PERMISSION,
                    )
                }
            }
        }

        compose.onNodeWithTag("info-sms-history-section").performClick()
        compose.onNodeWithTag("info-sms-history-section-dialog").assertIsDisplayed()
        val disclosure = compose.onNodeWithText(
            compose.activity.getString(R.string.peppy_sms_history_description),
            substring = true,
        )
        assertTrue(disclosure.fetchSemanticsNode().boundsInRoot.height > 0f)
    }
}
