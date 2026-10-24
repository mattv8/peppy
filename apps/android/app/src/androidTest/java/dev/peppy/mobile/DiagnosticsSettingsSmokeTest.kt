package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.state.ToggleableState
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DiagnosticsSettingsSmokeTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun switchBindsRecordingStateAndRetainedLogsStayActionableWhenOff() {
        var state by mutableStateOf(DiagnosticsState(enabled = false, bytes = 1024, dropped = 2))
        var requestedEnabled: Boolean? = null

        compose.setContent {
            DiagnosticsSettingsContent(
                state = state,
                onEnabledChanged = { requestedEnabled = it; state = state.copy(enabled = it) },
                onShare = {},
                onClear = { state = state.copy(bytes = 0) },
            )
        }

        compose.onNodeWithTag("diagnostics-verbose-sync-logging-switch").assertIsOff().performClick()
        compose.onNodeWithTag("diagnostics-verbose-sync-logging-switch").assertIsOn()
        compose.onNodeWithTag("diagnostics-share-log").assertIsEnabled()
        compose.onNodeWithTag("diagnostics-clear-logs").assertIsEnabled()
        assertEquals(true, requestedEnabled)
    }

    @Test
    fun switchRetainsItsAccessibleLabelRoleAndToggleState() {
        val label = compose.activity.getString(R.string.peppy_diagnostics_verbose_sync_logging)
        compose.setContent {
            DiagnosticsSettingsContent(
                state = DiagnosticsState(enabled = false),
                onEnabledChanged = {},
                onShare = {},
                onClear = {},
            )
        }

        compose.onNodeWithTag("diagnostics-verbose-sync-logging-switch")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.ContentDescription, listOf(label)))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.Switch))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.ToggleableState, ToggleableState.Off))
    }

    @Test
    fun busyStateDisablesControlsAndShowsTheControllerError() {
        compose.setContent {
            DiagnosticsSettingsContent(
                state = DiagnosticsState(bytes = 1024, busy = true, error = DiagnosticsError.EXPORT),
                onEnabledChanged = {},
                onShare = {},
                onClear = {},
            )
        }

        compose.onNodeWithTag("diagnostics-verbose-sync-logging-switch").assertIsNotEnabled()
        compose.onNodeWithTag("diagnostics-share-log").assertIsNotEnabled()
        compose.onNodeWithTag("diagnostics-clear-logs").assertIsNotEnabled()
        compose.onNodeWithTag("diagnostics-busy").assertIsDisplayed()
        compose.onNodeWithTag("diagnostics-error").assertIsDisplayed()
    }

    @Test
    fun clearRequiresConfirmationAndCancelLeavesRetainedLogs() {
        var state by mutableStateOf(DiagnosticsState(bytes = 1024))
        var clearCalls = 0

        compose.setContent {
            DiagnosticsSettingsContent(
                state = state,
                onEnabledChanged = {},
                onShare = {},
                onClear = { clearCalls += 1; state = state.copy(bytes = 0) },
            )
        }

        compose.onNodeWithTag("diagnostics-clear-logs").performClick()
        compose.onNodeWithTag("diagnostics-clear-cancel").performClick()
        compose.onNodeWithTag("diagnostics-clear-confirmation").assertDoesNotExist()
        assertEquals(0, clearCalls)
        compose.onNodeWithTag("diagnostics-clear-logs").performClick()
        compose.onNodeWithTag("diagnostics-clear-confirm").performClick()
        assertEquals(1, clearCalls)
    }
}
