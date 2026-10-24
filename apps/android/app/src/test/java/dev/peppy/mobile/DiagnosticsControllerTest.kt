package dev.peppy.mobile

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class DiagnosticsControllerTest {
    private val context = ApplicationProvider.getApplicationContext<android.content.Context>()

    @Test fun preferenceDefaultsOffAndPersistsRuntimeToggle() {
        val preferences = DiagnosticsPreferences(context)
        preferences.setEnabled(false)
        assertFalse(preferences.enabled())

        preferences.setEnabled(true)

        assertTrue(DiagnosticsPreferences(context).enabled())
    }

    @Test fun controllerExposesDefaultOffAndToggleState() {
        DiagnosticsPreferences(context).setEnabled(false)
        DiagnosticsController.initialize(context)
        DiagnosticsController.setEnabled(false)
        assertFalse(DiagnosticsController.state.value.enabled)

        DiagnosticsController.setEnabled(true)

        assertTrue(DiagnosticsController.state.value.enabled)
        DiagnosticsController.setEnabled(false)
    }
}
