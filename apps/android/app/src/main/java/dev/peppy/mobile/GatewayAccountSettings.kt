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
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Drop-in Account destination; parent calls GatewayAccountSettings(onChanged). */
@Composable
internal fun GatewayAccountSettings(onChanged: () -> Unit) {
    val context = LocalContext.current
    var state by remember { mutableStateOf<GatewayAccountState?>(null) }
    var gatewayStatus by remember { mutableStateOf<GatewayStatus?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var deleteOpen by remember { mutableStateOf(false) }
    var erase by remember { mutableStateOf("") }
    var revokeTarget by remember { mutableStateOf<GatewayAccountDevice?>(null) }
    val relayPrefs = remember { context.getSharedPreferences("peppy-relay", 0) }
    var relayEnabled by remember { mutableStateOf(relayPrefs.getBoolean("relay-enabled.v1", false)) }
    var relayOrigin by remember { mutableStateOf(relayPrefs.getString("relay-origin.v1", "") ?: "") }
    var remoteMessagingEnabled by remember { mutableStateOf(RemoteMessagingPreferences.enabled(context)) }
    val scope = rememberCoroutineScope()
    suspend fun refresh() {
        val loaded = withContext(Dispatchers.IO) { GatewayAccountHost.load(context) }
        gatewayStatus = withContext(Dispatchers.IO) { NativeGateway.status(context) }
        state = loaded
        if (state == null) failure = "Could not load devices. Unlock or check the server connection."
    }
    LaunchedEffect(Unit) { refresh() }
    // Lock and recovery may change while this screen remains open. This is local state only; the
    // explicit Refresh button remains the only roster/server request.
    LaunchedEffect(Unit) {
        while (true) {
            gatewayStatus = withContext(Dispatchers.IO) { NativeGateway.status(context) }
            delay(1_000)
        }
    }
    Column(Modifier.fillMaxWidth().testTag("account-tab-screen")) {
        val account = state
        if (account == null) Text(failure ?: "Loading account…", Modifier.testTag("devices-loading")) else {
            Section("identity-section", stringResource(R.string.peppy_account), showDivider = false) {
                StatusRow("identity-role", "Role", account.self.role)
                StatusRow("identity-device-id", "This device", account.self.id)
                if (gatewayStatus?.origin == "https://peppy.pro") {
                    OutlinedButton(onClick = {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://peppy.pro/account")))
                    }, modifier = Modifier.testTag("hosted-manage-account-button")) {
                        Text(stringResource(R.string.peppy_production_manage_account))
                    }
                }
                OutlinedButton(onClick = { scope.launch { failure = null; refresh(); onChanged() } }, modifier = Modifier.testTag("refresh-enrollment-button")) { Text("Refresh from server") }
            }
            Section("devices-section", stringResource(R.string.peppy_devices)) {
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
            if (account.self.role == "owner" || account.self.role == "gateway") {
                Section("remote-messaging-section", "Remote messaging") {
                    Row(Modifier.fillMaxWidth().testTag("remote-messaging-toggle-row")) {
                        Column(Modifier.weight(1f)) {
                            Text(stringResource(R.string.peppy_remote_messaging_label))
                            Text(stringResource(R.string.peppy_remote_messaging_hint))
                        }
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
                        OutlinedButton(
                            onClick = { context.findActivity()?.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0) },
                            modifier = Modifier.testTag("remote-messaging-notification-permission"),
                        ) { Text(stringResource(R.string.peppy_remote_messaging_allow_notifications)) }
                    }
                }
            }
            if (account.self.role == "owner") Section("vault-section", "Vault") {
                Button(onClick = { deleteOpen = true }, modifier = Modifier.testTag("vault-delete-button")) { Text("Delete vault") }
            }
        }
        Section("relay-health-section", "Wake relay", showDivider = account != null) {
            OutlinedTextField(relayOrigin, { relayOrigin = it }, label = { Text("Relay HTTPS origin") }, modifier = Modifier.testTag("relay-origin-field"))
            Row(Modifier.fillMaxWidth()) {
                Text("Enable wake relay", Modifier.weight(1f))
                Switch(relayEnabled, { enabled ->
                    relayEnabled = enabled
                    if (enabled) {
                        val origin = CredentialParser.canonicalOrigin(relayOrigin, false)
                        if (origin == null) { relayEnabled = false; failure = "Enter a canonical HTTPS relay origin." } else {
                            RelayWakeRuntime.setEnabled(context, origin, true)
                            if (context.resources.getIdentifier("google_app_id", "string", context.packageName) != 0) {
                                com.google.firebase.messaging.FirebaseMessaging.getInstance().token
                                    .addOnSuccessListener { RelayWakeRuntime.client?.register(it) }
                            }
                        }
                    } else { RelayWakeRuntime.setEnabled(context, null, false) }
                }, modifier = Modifier.testTag("relay-enable-toggle"))
            }
            Text(if (relayEnabled) "Relay registration waits for Firebase configuration." else "No relay configured. Sync runs while the app is open.", Modifier.testTag("relay-not-configured"))
        }
        failure?.let { Text(it, Modifier.testTag("account-error")) }
    }
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

private fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
