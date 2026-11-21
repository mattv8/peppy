package dev.peppy.mobile.settings

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.RelayWakeRuntime
import dev.peppy.mobile.diagnostics.DiagnosticsSettings
import dev.peppy.mobile.security.CredentialParser
import dev.peppy.mobile.ui.DetailPage
import dev.peppy.mobile.ui.DetailsStatusRow
import dev.peppy.mobile.ui.PageChrome
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.SettingsGroup
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun AdvancedSettingsPage(state: SettingsState, onBack: () -> Unit) {
    DetailPage(PageChrome(title = stringResource(R.string.peppy_settings_advanced), tag = "settings-advanced", onBack = onBack)) {
        ConnectionGroup(state)
        if (state.relayInAdvanced) CustomRelayGroup(state)
        DiagnosticsSettings()
    }
}

@Composable
private fun ConnectionGroup(state: SettingsState) {
    SettingsGroup(
        tag = "advanced-connection-group",
        title = stringResource(R.string.peppy_advanced_group_connection),
        showDivider = false,
    ) {
        DetailsStatusRow(
            "settings-server",
            stringResource(R.string.peppy_settings_server_section),
            state.origin ?: stringResource(R.string.peppy_advanced_not_enrolled),
        )
        Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                stringResource(R.string.peppy_encrypted),
                Modifier.testTag("settings-encryption"),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun CustomRelayGroup(state: SettingsState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    SettingsGroup(tag = "settings-relay-section", title = stringResource(R.string.peppy_advanced_group_relay)) {
        val invalidOriginText = stringResource(R.string.peppy_wake_relay_origin_invalid)
        val originError: (@Composable () -> Unit)? = if (state.relayOriginInvalid) {
            { Text(invalidOriginText, Modifier.testTag("relay-origin-error")) }
        } else {
            null
        }
        OutlinedTextField(
            value = state.relayOrigin,
            onValueChange = {
                state.relayOrigin = it
                state.relayOriginInvalid = false
            },
            enabled = state.relayReady,
            singleLine = true,
            isError = state.relayOriginInvalid,
            label = { Text(stringResource(R.string.peppy_wake_relay_origin_field)) },
            supportingText = originError,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth().testTag("relay-origin-field"),
        )
        SettingToggleRow(
            title = stringResource(R.string.peppy_wake_relay_enable),
            checked = state.relayEnabled,
            enabled = state.relayReady,
            onCheckedChange = { enabled ->
                val origin = if (enabled) CredentialParser.canonicalOrigin(state.relayOrigin, false) else null
                if (enabled && origin == null) {
                    state.relayOriginInvalid = true
                } else {
                    state.relayOriginInvalid = false
                    state.relayWritePending = true
                    scope.launch {
                        try {
                            withContext(Dispatchers.IO) { RelayWakeRuntime.setEnabled(context, origin, enabled) }
                            state.relayEnabled = withContext(Dispatchers.IO) { state.relayPrefs.getBoolean(RELAY_ENABLED, false) }
                        } finally {
                            state.relayWritePending = false
                        }
                    }
                }
            },
            tag = "relay-enable-toggle",
            subtitle = stringResource(
                if (state.relayEnabled) R.string.peppy_wake_community_relay_enabled else R.string.peppy_wake_community_relay_disabled,
            ),
        )
    }
}
