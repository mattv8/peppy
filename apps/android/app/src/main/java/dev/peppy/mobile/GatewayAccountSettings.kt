package dev.peppy.mobile

import android.content.Intent
import android.content.Context
import android.content.ContextWrapper
import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import android.net.Uri
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.ScreenBody
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.hostedWakeRelayUrl

/** Drop-in Account destination; parent calls GatewayAccountSettings(onChanged). */
@Composable
internal fun GatewayAccountSettings(
    onChanged: () -> Unit,
    onOpenSettings: () -> Unit = {},
    onAddPhone: () -> Unit = {},
    canAddPhone: Boolean = false,
    onConnectComputer: () -> Unit = {},
    canConnectComputer: Boolean = false,
    settingsMode: Boolean = false,
    onDetailChanged: (Boolean) -> Unit = {},
) {
    val context = LocalContext.current
    var state by remember { mutableStateOf<GatewayAccountState?>(null) }
    var gatewayStatus by remember { mutableStateOf<GatewayStatus?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var deleteOpen by remember { mutableStateOf(false) }
    var erase by remember { mutableStateOf("") }
    var revokeTarget by remember { mutableStateOf<GatewayAccountDevice?>(null) }
    var devicesOpen by remember { mutableStateOf(false) }
    var notificationConsentOpen by remember { mutableStateOf(false) }
    val relayPrefs = remember { context.getSharedPreferences("peppy-relay", 0) }
    var relayEnabled by remember { mutableStateOf(relayPrefs.getBoolean("relay-enabled.v1", false)) }
    var relayOrigin by remember { mutableStateOf(relayPrefs.getString("relay-origin.v1", "") ?: "") }
    var remoteMessagingEnabled by remember { mutableStateOf(RemoteMessagingPreferences.enabled(context)) }
    var wakePreference by remember { mutableStateOf(RelayWakeRuntime.preference(context)) }
    var wakeStatus by remember { mutableStateOf(RelayWakeRuntime.status(context)) }
    val scope = rememberCoroutineScope()
    DisposableEffect(relayPrefs, context) {
        val listener = android.content.SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            when (key) {
                "hosted-relay-preference.v1" -> wakePreference = RelayWakeRuntime.preference(context)
                "hosted-relay-status.v1" -> wakeStatus = RelayWakeRuntime.status(context)
                "relay-enabled.v1" -> relayEnabled = relayPrefs.getBoolean("relay-enabled.v1", false)
                "relay-origin.v1" -> relayOrigin = relayPrefs.getString("relay-origin.v1", "") ?: ""
            }
        }
        relayPrefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { relayPrefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    suspend fun refresh() {
        val loaded = withContext(Dispatchers.IO) { GatewayAccountHost.load(context) }
        gatewayStatus = withContext(Dispatchers.IO) { NativeGateway.status(context) }
        state = loaded
        if (state == null) failure = "Could not load devices. Unlock or check the server connection."
    }
    LaunchedEffect(Unit) { refresh() }
    DisposableEffect(devicesOpen) {
        onDetailChanged(devicesOpen)
        onDispose { onDetailChanged(false) }
    }
    BackHandler(enabled = devicesOpen) { devicesOpen = false }
    // Lock and recovery may change while this screen remains open. This is local state only; the
    // explicit Refresh button remains the only roster/server request.
    LaunchedEffect(Unit) {
        while (true) {
            gatewayStatus = withContext(Dispatchers.IO) { NativeGateway.status(context) }
            delay(1_000)
        }
    }
    Column(Modifier.fillMaxWidth().testTag(if (settingsMode) "settings-account-group" else if (devicesOpen) "devices-screen" else "account-tab-screen")) {
        val account = state
        if (account == null) {
            if (!settingsMode) {
                HeroBanner(stringResource(R.string.peppy_toggle_android_account_offline_title), gatewayStatus?.origin ?: stringResource(R.string.peppy_toggle_android_account_offline_body), "account-hero")
                ScreenBody("account-offline-body") {
                    Text(failure ?: stringResource(R.string.peppy_toggle_android_account_loading), Modifier.testTag("devices-loading"))
                    TextButton(onClick = { scope.launch { failure = null; refresh() } }, modifier = Modifier.testTag("account-retry-button")) { Text(stringResource(R.string.peppy_toggle_android_retry)) }
                    AccountNavigationRow("account-settings-row", stringResource(R.string.peppy_toggle_android_account_settings), onOpenSettings)
                    AccountNavigationRow("account-privacy-policy-row", stringResource(R.string.peppy_privacy_policy)) { openUri(context, "https://peppy.pro/privacy", failureSetter = { failure = it }) }
                }
            }
        } else {
            if (!settingsMode && !devicesOpen) {
                HeroBanner(
                    title = stringResource(R.string.peppy_account),
                    body = listOfNotNull(gatewayStatus?.origin, stringResource(R.string.peppy_toggle_android_account_role, account.self.role)).joinToString("\n"),
                    tag = "account-hero",
                )
                ScreenBody("account-body") {
                Section("account-section", stringResource(R.string.peppy_account), showDivider = false) {
                    AccountNavigationRow("account-devices-row", stringResource(R.string.peppy_toggle_android_account_devices)) { devicesOpen = true }
                    if (AccountSettingsPolicy.canAddPhone(account.self.role, canAddPhone)) {
                        AccountNavigationRow("add-phone-row", stringResource(R.string.peppy_toggle_android_account_add_phone), onAddPhone)
                    }
                    if (canConnectComputer) {
                        AccountNavigationRow("connect-computer-row", stringResource(R.string.peppy_owner_computer_add_cta), onConnectComputer)
                    } else {
                        Text(
                            stringResource(R.string.peppy_owner_computer_owner_required),
                            modifier = Modifier.testTag("connect-computer-owner-help"),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    if (gatewayStatus?.origin == "https://peppy.pro") {
                        AccountNavigationRow("account-hosted-row", stringResource(R.string.peppy_toggle_android_account_billing)) {
                        openUri(context, "https://peppy.pro/account", failureSetter = { failure = it })
                        }
                    }
                    AccountNavigationRow("account-settings-row", stringResource(R.string.peppy_toggle_android_account_settings), onOpenSettings)
                    AccountNavigationRow("account-privacy-policy-row", stringResource(R.string.peppy_privacy_policy)) {
                        openUri(context, "https://peppy.pro/privacy", failureSetter = { failure = it })
                    }
                }
                }
            }
            if (devicesOpen) {
                ScreenBody("devices-body") {
                TextButton(onClick = { devicesOpen = false }, modifier = Modifier.testTag("devices-back-button")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.peppy_back)) }
                Section("devices-section", stringResource(R.string.peppy_devices), showDivider = false) {
                account.devices.forEach { device ->
                    Row(Modifier.fillMaxWidth().testTag("device-row-${device.id}")) {
                        Text("${device.id.take(8)} · ${device.role}${if (device.id == account.self.id) " (this device)" else ""}", Modifier.weight(1f))
                        if (!device.revoked && (device.id == account.self.id || account.self.role == "owner")) OutlinedButton(
                            onClick = { revokeTarget = device },
                            modifier = Modifier.testTag("device-remove-${device.id}"),
                        ) { Text(if (device.id == account.self.id) "Disconnect" else "Remove") }
                    }
                }
                }
                TextButton(onClick = { scope.launch { failure = null; refresh(); onChanged() } }, modifier = Modifier.testTag("refresh-enrollment-button")) { Text("Refresh from server") }
                }
            }
            if (settingsMode && gatewayStatus?.sharedKeysReady == true && (account.self.role == "owner" || account.self.role == "gateway")) {
                Section("settings-sync-section", stringResource(R.string.peppy_toggle_android_settings_sync)) {
                    Row(Modifier.fillMaxWidth().testTag("remote-messaging-toggle-row")) {
                        Column(Modifier.weight(1f)) {
                            Text(stringResource(R.string.peppy_remote_messaging_label))
                        }
                        InfoButton(
                            topic = stringResource(R.string.peppy_remote_messaging_label),
                            title = stringResource(R.string.peppy_remote_messaging_label),
                            body = stringResource(R.string.peppy_remote_messaging_hint),
                            tag = "info-remote-messaging",
                        )
                        Switch(
                            checked = remoteMessagingEnabled,
                            enabled = gatewayStatus?.sharedKeysReady == true && gatewayStatus?.syncPhase != SyncPhase.RECOVERY_REQUIRED,
                            onCheckedChange = { enabled ->
                                remoteMessagingEnabled = enabled
                                RemoteMessagingPreferences.setEnabled(context, enabled)
                                if (enabled) RemoteMessagingService.resumeOrEnqueue(context) else RemoteMessagingService.stop(context)
                            },
                            modifier = Modifier.testTag("remote-messaging-toggle"),
                        )
                    }
                    val statusRes = when (RemoteMessagingRuntime.status()) {
                        RemoteMessagingStatus.CONNECTED -> R.string.peppy_remote_messaging_status_connected
                        RemoteMessagingStatus.RECONNECTING -> R.string.peppy_remote_messaging_status_reconnecting
                        RemoteMessagingStatus.LOCKED -> R.string.peppy_remote_messaging_status_locked
                    }
                    if (remoteMessagingEnabled) StatusRow("remote-messaging-status", stringResource(R.string.peppy_remote_messaging_status_label), stringResource(statusRes))
                    if (gatewayStatus?.syncPhase == SyncPhase.RECOVERY_REQUIRED) Text(stringResource(R.string.peppy_remote_messaging_recovery_required))
                    else if (gatewayStatus?.sharedKeysReady != true) Text(stringResource(R.string.peppy_remote_messaging_status_locked))
                    if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                        Text(stringResource(R.string.peppy_remote_messaging_notifications_disabled))
                        TextButton(
                            onClick = { notificationConsentOpen = true },
                            modifier = Modifier.testTag("remote-messaging-notification-permission"),
                        ) { Text(stringResource(R.string.peppy_remote_messaging_allow_notifications)) }
                    }
                }
            }
        }
        if (settingsMode && gatewayStatus?.sharedKeysReady == true) Section("settings-relay-section", stringResource(R.string.peppy_wake_section_title), showDivider = account != null) {
            val hostedRelay = NativeGateway.accountCredential(context)?.let { hostedWakeRelayUrl(it.origin) }
            if (hostedRelay != null) {
                val statusText = when (wakeStatus) {
                    RelayWakeStatus.REGISTERED -> stringResource(R.string.peppy_wake_status_registered)
                    RelayWakeStatus.RETRYING -> stringResource(R.string.peppy_wake_status_retrying)
                    RelayWakeStatus.PAUSED -> stringResource(R.string.peppy_wake_status_paused)
                    RelayWakeStatus.UNAVAILABLE -> stringResource(R.string.peppy_wake_status_unavailable)
                    RelayWakeStatus.PENDING -> stringResource(R.string.peppy_wake_status_pending)
                    RelayWakeStatus.MISSING_APP_SUPPORT -> stringResource(R.string.peppy_wake_status_unavailable)
                    RelayWakeStatus.CLEANUP_PENDING -> stringResource(R.string.peppy_wake_status_paused)
                }
                val footer = when (wakeStatus) {
                    RelayWakeStatus.REGISTERED -> R.string.peppy_wake_footer_registered
                    RelayWakeStatus.RETRYING -> R.string.peppy_wake_footer_retrying
                    RelayWakeStatus.PAUSED -> R.string.peppy_wake_footer_paused
                    RelayWakeStatus.UNAVAILABLE -> R.string.peppy_wake_footer_unavailable_server
                    RelayWakeStatus.PENDING -> R.string.peppy_wake_footer_pending
                    RelayWakeStatus.MISSING_APP_SUPPORT -> R.string.peppy_wake_footer_missing_app_support
                    RelayWakeStatus.CLEANUP_PENDING -> R.string.peppy_wake_cleanup_pending
                }
                StatusRow("relay-wake-status", stringResource(R.string.peppy_wake_status_label), statusText)
                Row(Modifier.fillMaxWidth()) {
                    Text(stringResource(R.string.peppy_wake_updates_toggle_label), Modifier.weight(1f).testTag("relay-wake-toggle-label"))
                    Switch(wakePreference != RelayWakePreference.PAUSED, { enabled ->
                        wakePreference = if (enabled) RelayWakePreference.AUTOMATIC else RelayWakePreference.PAUSED
                        RelayWakeRuntime.setPreference(context, wakePreference)
                    }, modifier = Modifier.testTag("relay-wake-toggle"))
                }
                Text(stringResource(footer), Modifier.testTag("relay-wake-footer"))
            } else {
                OutlinedTextField(relayOrigin, { relayOrigin = it }, label = { Text("Relay HTTPS origin") }, modifier = Modifier.testTag("relay-origin-field"))
                Row(Modifier.fillMaxWidth()) {
                    Text("Enable wake relay", Modifier.weight(1f))
                    Switch(relayEnabled, { enabled ->
                        relayEnabled = enabled
                        if (enabled) {
                            val origin = CredentialParser.canonicalOrigin(relayOrigin, false)
                            if (origin == null) { relayEnabled = false; failure = "Enter a canonical HTTPS relay origin." } else {
                                RelayWakeRuntime.setEnabled(context, origin, true)
                            }
                        } else { RelayWakeRuntime.setEnabled(context, null, false) }
                    }, modifier = Modifier.testTag("relay-enable-toggle"))
                }
                val communityFooter = if (relayEnabled) R.string.peppy_wake_community_relay_enabled else R.string.peppy_wake_community_relay_disabled
                Text(stringResource(communityFooter), Modifier.testTag("relay-not-configured"))
            }
        }
        if (settingsMode && account?.self?.role == "owner") Section("settings-group-danger", stringResource(R.string.peppy_toggle_android_settings_danger)) {
            Button(onClick = { deleteOpen = true }, modifier = Modifier.testTag("vault-delete-button")) { Text("Delete vault") }
        }
        failure?.let { Text(it, Modifier.testTag("account-error")) }
    }
    if (notificationConsentOpen) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_settings_notifications_title),
        body = stringResource(R.string.peppy_toggle_android_settings_notifications_rationale),
        tag = "remote-messaging-notification-consent",
        onDismiss = { notificationConsentOpen = false },
        onConfirm = {
            notificationConsentOpen = false
            if (Build.VERSION.SDK_INT >= 33) context.findActivity()?.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        },
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
    )
    if (deleteOpen) AlertDialog(
        onDismissRequest = { deleteOpen = false }, title = { Text("Type ERASE to confirm") },
        text = { OutlinedTextField(erase, { erase = it }, label = { Text("Type ERASE") }, modifier = Modifier.testTag("vault-delete-confirm-field")) },
        confirmButton = { Button(onClick = { scope.launch { val ok = withContext(Dispatchers.IO) { GatewayAccountHost.deleteVault(context) }; if (!ok) failure = "Could not reach the server. The vault was not deleted." else onChanged() }; deleteOpen = false }, enabled = erase == "ERASE", modifier = Modifier.testTag("vault-delete-final-button")) { Text("Delete vault") } },
        dismissButton = { OutlinedButton(onClick = { deleteOpen = false }) { Text("Keep vault") } },
    )
    revokeTarget?.let { target ->
        val own = state?.self?.id == target.id
        AlertDialog(
            onDismissRequest = { revokeTarget = null },
            title = { Text(if (own) "Disconnect this phone?" else "Remove device?") },
            text = { Text(if (own) "This stops Peppy locally. If offline, the server credential remains active until a later revoke succeeds." else "This device will lose access to the Peppy server.") },
            confirmButton = { Button(onClick = {
                scope.launch {
                    val ok = withContext(Dispatchers.IO) { GatewayAccountHost.revoke(context, target.id) }
                    if (!ok) failure = if (own) "Could not revoke. Peppy remains locally available and the server token is still active." else "Could not remove device."
                    else { revokeTarget = null; refresh(); onChanged() }
                }
            }, modifier = Modifier.testTag("device-revoke-confirm-${target.id}")) { Text(if (own) "Disconnect" else "Remove") } },
            dismissButton = { OutlinedButton(onClick = { revokeTarget = null }) { Text("Cancel") } },
        )
    }
}

private fun openUri(context: Context, uri: String, failureSetter: (String) -> Unit) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri)))
    } catch (_: android.content.ActivityNotFoundException) {
        failureSetter(context.getString(R.string.peppy_toggle_android_link_unavailable))
    }
}

private fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}

@Composable
private fun AccountNavigationRow(tag: String, label: String, onClick: () -> Unit) {
    TextButton(onClick = onClick, modifier = Modifier.fillMaxWidth().testTag(tag), colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onSurface)) {
        Text(label, Modifier.weight(1f))
        Text("›", Modifier.clearAndSetSemantics {})
    }
}
