package dev.peppy.mobile

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
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SmsSettingsDisclosureTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun fullDisclosureIsVisibleBeforeSmsPermissionControlAtNormalFont() {
        assertDisclosurePrecedesPermissionControl(fontScale = 1f)
    }

    @Test
    fun fullDisclosureIsVisibleBeforeSmsPermissionControlAtLargeFont() {
        assertDisclosurePrecedesPermissionControl(fontScale = 1.3f)
    }

    private fun assertDisclosurePrecedesPermissionControl(fontScale: Float) {
        var permissionRequests = 0
        var settingsRequests = 0
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    SmsSettings(
                        context = LocalContext.current,
                        historyState = SmsHistoryState.WAITING_FOR_PERMISSION,
                        needsPermissionSettings = false,
                        requestPermission = { permissionRequests += 1 },
                        openPermissionSettings = { settingsRequests += 1 },
                    )
                }
            }
        }

        val disclosure = compose.onNodeWithTag("sms-history-disclosure")
        val permission = compose.onNodeWithTag("sms-read-permission-button")
        disclosure.performScrollTo().assertIsDisplayed()
        permission.performScrollTo().assertIsDisplayed()
        assertTrue(
            "SMS history disclosure must precede permission control",
            disclosure.fetchSemanticsNode().boundsInRoot.top < permission.fetchSemanticsNode().boundsInRoot.top,
        )
        assertEquals(0, permissionRequests)
        assertEquals(0, settingsRequests)
    }
}
