package dev.peppy.mobile.restrictedsetup.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import dev.peppy.mobile.R
import dev.peppy.mobile.restrictedsetup.CodeSubmitResult
import dev.peppy.mobile.restrictedsetup.RestrictedSetupAccess
import dev.peppy.mobile.restrictedsetup.RestrictedSetupFailure
import dev.peppy.mobile.restrictedsetup.RestrictedSetupHint
import dev.peppy.mobile.restrictedsetup.RestrictedSetupOptions
import dev.peppy.mobile.restrictedsetup.RestrictedSetupStage
import dev.peppy.mobile.restrictedsetup.RestrictedSetupState
import dev.peppy.mobile.restrictedsetup.WirelessDebuggingOutcome
import dev.peppy.mobile.sms.contextActivity
import dev.peppy.mobile.ui.ConsentSheet

@Composable
internal fun RestrictedSetupPanel(
    state: RestrictedSetupState,
    onAccessChanged: () -> Unit,
    modifier: Modifier = Modifier,
    begin: (RestrictedSetupOptions) -> Boolean,
    submitCode: (CharArray, Int?) -> CodeSubmitResult,
    submitConnectPort: (Int) -> Boolean,
    showCodePromptAgain: () -> Unit,
    cancel: () -> Unit,
    acknowledgeCleanup: () -> Boolean,
    startService: () -> Unit,
) {
    var showConsent by remember { mutableStateOf(false) }
    var turnOffWirelessDebugging by rememberSaveable { mutableStateOf(false) }
    var fallbackShown by rememberSaveable { mutableStateOf(false) }
    var cancelled by remember { mutableStateOf(false) }
    var accessDelivered by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(state.stage, state.access) {
        if (state.stage == RestrictedSetupStage.COMPLETE && state.access == RestrictedSetupAccess.ALLOWED && !accessDelivered) {
            accessDelivered = true
            onAccessChanged()
        }
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        HorizontalDivider()
        if (cancelled) Text(stringResource(R.string.peppy_restricted_setup_cancelled), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
        when (state.stage) {
            RestrictedSetupStage.INTRODUCTION, RestrictedSetupStage.CLEANUP_REQUIRED -> if (state.access != RestrictedSetupAccess.ALLOWED) IdleCard(turnOffWirelessDebugging, { turnOffWirelessDebugging = it }, { showConsent = true })
            RestrictedSetupStage.AWAITING_PAIRING -> PairingPanel(state, fallbackShown, { fallbackShown = it }, submitCode, showCodePromptAgain, { cancelled = true; cancel() })
            RestrictedSetupStage.PAIRING, RestrictedSetupStage.CONNECTING, RestrictedSetupStage.APPLYING, RestrictedSetupStage.VERIFYING, RestrictedSetupStage.TURNING_OFF -> ProgressPanel(state, submitConnectPort, { cancelled = true; cancel() })
            RestrictedSetupStage.COMPLETE -> CompletePanel(state)
            RestrictedSetupStage.FAILED -> FailedPanel(state, { showConsent = true })
        }
        if (state.cleanupNeeded && state.stage in setOf(RestrictedSetupStage.COMPLETE, RestrictedSetupStage.CLEANUP_REQUIRED, RestrictedSetupStage.FAILED)) CleanupCard(state, acknowledgeCleanup)
    }
    if (showConsent) Consent(
        turnOffWirelessDebugging = turnOffWirelessDebugging,
        onDismiss = { showConsent = false },
        onConfirm = {
            showConsent = false
            if (begin(RestrictedSetupOptions(turnOffWirelessDebugging))) { cancelled = false; startService() }
        },
    )
}

@Composable
private fun IdleCard(checked: Boolean, onCheckedChange: (Boolean) -> Unit, onStart: () -> Unit) {
    Column(Modifier.testTag("restricted-setup-idle"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Heading(R.string.peppy_restricted_setup_title)
        Text(stringResource(R.string.peppy_restricted_setup_idle_body))
        Text(stringResource(R.string.peppy_restricted_setup_idle_bridge), style = MaterialTheme.typography.bodySmall)
        Row(
            Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-wifi-off-option")
                .toggleable(checked, role = Role.Checkbox, onValueChange = onCheckedChange),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Checkbox(checked = checked, onCheckedChange = null)
            Column(Modifier.padding(start = 8.dp)) {
                Text(stringResource(R.string.peppy_restricted_setup_wifi_off_option))
                Text(stringResource(R.string.peppy_restricted_setup_wifi_off_hint), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall)
            }
        }
        Button(onClick = onStart, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-start")) { Text(stringResource(R.string.peppy_restricted_setup_start)) }
    }
}

@Composable
private fun Consent(turnOffWirelessDebugging: Boolean, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    val context = LocalContext.current
    val activity = context.contextActivity()
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { onConfirm() }
    val command = remember(context.packageName) { "appops set ${context.packageName} READ_RESTRICTED_MESSAGES allow" }
    val wifiOffCommand = "settings put global adb_wifi_enabled 0"
    val commandParagraph = buildString {
        append(stringResource(R.string.peppy_restricted_setup_consent_p_command))
        append("\n")
        append(command)
        if (turnOffWirelessDebugging) {
            append("\n\n")
            append(stringResource(R.string.peppy_restricted_setup_consent_p_command_wifi_off))
            append("\n")
            append(wifiOffCommand)
        }
    }
    val paragraphs = listOf(
        stringResource(R.string.peppy_restricted_setup_consent_p_power),
        commandParagraph,
        stringResource(R.string.peppy_restricted_setup_consent_p_notification),
        stringResource(R.string.peppy_restricted_setup_consent_p_scope),
        stringResource(R.string.peppy_restricted_setup_consent_p_pairing_stays),
        stringResource(R.string.peppy_restricted_setup_consent_p_memory),
        stringResource(R.string.peppy_restricted_setup_consent_p_distribution),
    ).joinToString("\n\n")
    ConsentSheet(
        title = stringResource(R.string.peppy_restricted_setup_consent_title), body = paragraphs, tag = "restricted-setup-consent-sheet",
        onDismiss = onDismiss,
        onConfirm = {
            if (Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) permission.launch(Manifest.permission.POST_NOTIFICATIONS) else onConfirm()
        },
        confirmLabel = stringResource(R.string.peppy_restricted_setup_consent_confirm),
        disclosureTag = "restricted-setup-disclosure", showPrivacyPolicy = false, dismissLabel = stringResource(R.string.peppy_toggle_android_not_now),
    )
}

@Composable
private fun PairingPanel(state: RestrictedSetupState, fallbackShown: Boolean, onFallbackShown: (Boolean) -> Unit, submitCode: (CharArray, Int?) -> CodeSubmitResult, resend: () -> Unit, cancel: () -> Unit) {
    var port by remember(state.pairingPort) { mutableStateOf(state.pairingPort?.toString().orEmpty()) }
    var code by remember { mutableStateOf("") }
    Column(Modifier.testTag("restricted-setup-pairing"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Heading(R.string.peppy_restricted_setup_pairing_title)
        DiscoveryStatus(state)
        state.hint?.let { Hint(it) }
        Step(1, R.string.peppy_restricted_setup_step_1)
        SettingsButton("restricted-setup-open-settings")
        Step(2, R.string.peppy_restricted_setup_step_2)
        Step(3, if (state.notificationAvailable) R.string.peppy_restricted_setup_step_3_notification else R.string.peppy_restricted_setup_step_3_inapp)
        Text(stringResource(R.string.peppy_restricted_setup_dev_options_hint), style = MaterialTheme.typography.bodySmall)
        if (!state.notificationAvailable) Text(stringResource(R.string.peppy_restricted_setup_notifications_off_hint), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
        if (!fallbackShown && state.notificationAvailable) TextButton(onClick = { onFallbackShown(true) }, modifier = Modifier.heightIn(min = 48.dp).testTag("restricted-setup-code-fallback-toggle")) { Text(stringResource(R.string.peppy_restricted_setup_code_fallback_toggle)) }
        if (fallbackShown || !state.notificationAvailable || state.discovery == dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery.TIMED_OUT) {
            FallbackForm(port, { port = it }, code, { code = it }, submitCode) { code = "" }
            Text(stringResource(R.string.peppy_restricted_setup_split_screen_hint), Modifier.testTag("restricted-setup-split-hint"), style = MaterialTheme.typography.bodySmall)
        }
        if (state.notificationAvailable && state.pairingPort != null) TextButton(onClick = resend, modifier = Modifier.heightIn(min = 48.dp).testTag("restricted-setup-notification-resend")) { Text(stringResource(R.string.peppy_restricted_setup_notification_resend)) }
        CancelButton(cancel, false)
    }
}

@Composable
private fun FallbackForm(port: String, onPortChanged: (String) -> Unit, code: String, onCodeChanged: (String) -> Unit, submitCode: (CharArray, Int?) -> CodeSubmitResult, clearCode: () -> Unit) {
    var error by remember { mutableStateOf(false) }
    val parsedPort = parsePort(port)
    Column(Modifier.testTag("restricted-setup-code-fallback")) {
        OutlinedTextField(port, onPortChanged, Modifier.fillMaxWidth().testTag("restricted-setup-pair-port"), label = { Text(stringResource(R.string.peppy_restricted_setup_pair_port_label)) }, supportingText = { Text(stringResource(R.string.peppy_restricted_setup_pair_port_hint)) }, keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Next))
        OutlinedTextField(code, { onCodeChanged(it.filter(Char::isDigit).take(6)) }, Modifier.fillMaxWidth().testTag("restricted-setup-pair-code"), label = { Text(stringResource(R.string.peppy_restricted_setup_pair_code_label)) }, supportingText = { Text(stringResource(R.string.peppy_restricted_setup_pair_code_hint)) }, visualTransformation = PasswordVisualTransformation(), keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Done))
        Button(onClick = { val result = submitCode(code.toCharArray(), parsedPort); error = result != CodeSubmitResult.ACCEPTED; clearCode() }, enabled = code.length == 6 && parsedPort != null, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-pair")) { Text(stringResource(R.string.peppy_restricted_setup_pair_action)) }
        if (error) Text(stringResource(R.string.peppy_restricted_setup_submit_unavailable), Modifier.semantics { liveRegion = LiveRegionMode.Assertive })
    }
}

internal fun parsePort(value: String): Int? = value.substringAfterLast(':').filter(Char::isDigit).toIntOrNull()?.takeIf { it in 1..65535 }

@Composable
private fun DiscoveryStatus(state: RestrictedSetupState) {
    val text = when (state.discovery) {
        dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery.SEARCHING -> R.string.peppy_restricted_setup_discovery_searching
        dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery.FOUND -> if (state.notificationAvailable) R.string.peppy_restricted_setup_discovery_found_notification else R.string.peppy_restricted_setup_discovery_found_inapp
        else -> R.string.peppy_restricted_setup_discovery_none
    }
    Row(Modifier.testTag("restricted-setup-discovery-status"), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        if (state.discovery == dev.peppy.mobile.restrictedsetup.RestrictedSetupDiscovery.SEARCHING) CircularProgressIndicator(Modifier.size(20.dp).clearAndSetSemantics {})
        Text(stringResource(text), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
    }
}

@Composable
private fun Hint(hint: RestrictedSetupHint) {
    val text = when (hint) {
        RestrictedSetupHint.NO_WIFI -> R.string.peppy_restricted_setup_hint_no_wifi
        RestrictedSetupHint.CODE_REJECTED -> R.string.peppy_restricted_setup_hint_code_rejected
        RestrictedSetupHint.PAIRING_BOX_CLOSED -> R.string.peppy_restricted_setup_hint_box_closed
    }
    Text(stringResource(text), Modifier.testTag("restricted-setup-hint").semantics { liveRegion = LiveRegionMode.Assertive })
}

@Composable
private fun Step(number: Int, string: Int) = Text(stringResource(string), Modifier.testTag("restricted-setup-step-$number"))

@Composable
private fun ProgressPanel(state: RestrictedSetupState, submitConnectPort: (Int) -> Boolean, cancel: () -> Unit) {
    var port by remember { mutableStateOf("") }
    val status = when (state.stage) {
        RestrictedSetupStage.PAIRING -> R.string.peppy_restricted_setup_status_pairing
        RestrictedSetupStage.CONNECTING -> R.string.peppy_restricted_setup_status_connecting
        RestrictedSetupStage.APPLYING -> R.string.peppy_restricted_setup_status_applying
        RestrictedSetupStage.VERIFYING -> R.string.peppy_restricted_setup_status_verifying
        else -> R.string.peppy_restricted_setup_status_turning_off
    }
    Column(Modifier.testTag("restricted-setup-progress"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        CircularProgressIndicator()
        Text(stringResource(status), Modifier.testTag("restricted-setup-status").semantics { liveRegion = LiveRegionMode.Polite })
        if (state.needsConnectPort) {
            Text(stringResource(R.string.peppy_restricted_setup_connect_port_prompt))
            OutlinedTextField(port, { port = it }, Modifier.fillMaxWidth().testTag("restricted-setup-connect-port"), label = { Text(stringResource(R.string.peppy_restricted_setup_connect_port_label)) }, keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = KeyboardType.Number))
            Button(onClick = { parsePort(port)?.let(submitConnectPort) }, enabled = parsePort(port) != null, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-connect")) { Text(stringResource(R.string.peppy_restricted_setup_connect_action)) }
        }
        CancelButton(cancel, state.stage == RestrictedSetupStage.VERIFYING || state.stage == RestrictedSetupStage.TURNING_OFF)
    }
}

@Composable
private fun CompletePanel(state: RestrictedSetupState) {
    Column(Modifier.testTag("restricted-setup-complete"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.peppy_restricted_setup_complete_title), Modifier.semantics { heading(); liveRegion = LiveRegionMode.Polite }, style = MaterialTheme.typography.titleMedium)
        Text(stringResource(R.string.peppy_restricted_setup_complete_body))
        Text(stringResource(R.string.peppy_restricted_setup_checked_when_finished), style = MaterialTheme.typography.bodySmall)
        when (state.wirelessDebugging) {
            WirelessDebuggingOutcome.OFF -> Text(stringResource(R.string.peppy_restricted_setup_complete_wifi_off_confirmed))
            WirelessDebuggingOutcome.REQUESTED -> Text(stringResource(R.string.peppy_restricted_setup_complete_wifi_off_requested))
            else -> Unit
        }
    }
}

@Composable
private fun FailedPanel(state: RestrictedSetupState, retry: () -> Unit) {
    val error = when (state.failure) {
        RestrictedSetupFailure.PAIRING_FAILED -> R.string.peppy_restricted_setup_error_pairing_failed
        RestrictedSetupFailure.CONNECT_FAILED -> R.string.peppy_restricted_setup_error_connect_failed
        RestrictedSetupFailure.INVALID_ENDPOINT -> R.string.peppy_restricted_setup_error_endpoint
        RestrictedSetupFailure.TIMED_OUT -> R.string.peppy_restricted_setup_error_timeout
        RestrictedSetupFailure.CLEANUP_MARKER -> R.string.peppy_restricted_setup_error_cleanup_marker
        RestrictedSetupFailure.NOT_VERIFIED -> R.string.peppy_restricted_setup_error_not_verified
        else -> R.string.peppy_restricted_setup_error_transport
    }
    Column(Modifier.testTag("restricted-setup-failed"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(error), Modifier.testTag("restricted-setup-error").semantics { liveRegion = LiveRegionMode.Assertive })
        Button(onClick = retry, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-retry")) { Text(stringResource(R.string.peppy_restricted_setup_start_over)) }
    }
}

@Composable
private fun CleanupCard(state: RestrictedSetupState, acknowledge: () -> Boolean) {
    var forgotten by remember { mutableStateOf(false) }
    var wirelessOff by remember { mutableStateOf(false) }
    val needsWirelessConfirmation = state.wirelessDebugging != WirelessDebuggingOutcome.OFF
    Column(Modifier.testTag("restricted-setup-cleanup"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Heading(R.string.peppy_restricted_setup_cleanup_title)
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_body))
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_body_leftover))
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_step_1))
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_step_2))
        if (needsWirelessConfirmation) Text(stringResource(R.string.peppy_restricted_setup_cleanup_step_3_check))
        SettingsButton("restricted-setup-cleanup-open-settings")
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_checked_heading), Modifier.testTag("restricted-setup-cleanup-checked").semantics { heading() })
        Text("${stringResource(R.string.peppy_restricted_setup_cleanup_access)}: ${when (state.access) { RestrictedSetupAccess.ALLOWED -> stringResource(R.string.peppy_restricted_setup_cleanup_allowed); RestrictedSetupAccess.NOT_ALLOWED -> stringResource(R.string.peppy_restricted_setup_cleanup_not_allowed); else -> stringResource(R.string.peppy_restricted_setup_cleanup_unknown) }}", Modifier.testTag("restricted-setup-checked-access"))
        Text(stringResource(R.string.peppy_restricted_setup_cleanup_asserted_heading), Modifier.testTag("restricted-setup-cleanup-asserted").semantics { heading() })
        CleanupCheckbox("restricted-setup-confirm-forgotten", R.string.peppy_restricted_setup_confirm_forgotten, forgotten) { forgotten = it }
        if (needsWirelessConfirmation) CleanupCheckbox("restricted-setup-confirm-wireless-off", R.string.peppy_restricted_setup_confirm_wireless_off, wirelessOff) { wirelessOff = it }
        Button(onClick = { acknowledge() }, enabled = forgotten && (!needsWirelessConfirmation || wirelessOff), modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("restricted-setup-cleanup-acknowledge")) { Text(stringResource(R.string.peppy_restricted_setup_cleanup_acknowledge)) }
    }
}

@Composable
private fun CleanupCheckbox(tag: String, label: Int, checked: Boolean, onCheckedChange: (Boolean) -> Unit) = Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag).toggleable(checked, role = Role.Checkbox, onValueChange = onCheckedChange)) { Checkbox(checked, null); Text(stringResource(label), Modifier.padding(start = 8.dp)) }

@Composable
private fun CancelButton(cancel: () -> Unit, disabled: Boolean) = TextButton(onClick = cancel, enabled = !disabled, modifier = Modifier.heightIn(min = 48.dp).testTag("restricted-setup-cancel")) { Text(stringResource(if (disabled) R.string.peppy_restricted_setup_checking else R.string.peppy_cancel)) }

@Composable
private fun Heading(string: Int) = Text(stringResource(string), Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)

@Composable
private fun SettingsButton(tag: String) {
    val context = LocalContext.current
    val activity = context.contextActivity()
    var unavailable by remember { mutableStateOf(false) }
    var developerOptionsOff by remember { mutableStateOf(false) }
    TextButton(onClick = {
        try {
            developerOptionsOff = Settings.Global.getInt(
                context.contentResolver,
                Settings.Global.DEVELOPMENT_SETTINGS_ENABLED,
                0,
            ) == 0
            val intent = android.content.Intent(
                if (developerOptionsOff) Settings.ACTION_DEVICE_INFO_SETTINGS else Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS,
            )
            if (activity?.isInMultiWindowMode == true) intent.addFlags(android.content.Intent.FLAG_ACTIVITY_LAUNCH_ADJACENT or android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
            (activity ?: context).startActivity(intent)
        } catch (_: Exception) { unavailable = true }
    }, modifier = Modifier.heightIn(min = 48.dp).testTag(tag)) { Text(stringResource(R.string.peppy_restricted_setup_open_developer_options)) }
    if (developerOptionsOff) Text(stringResource(R.string.peppy_restricted_setup_developer_options_off), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
    if (unavailable) Text(stringResource(R.string.peppy_restricted_setup_settings_unavailable), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
}
