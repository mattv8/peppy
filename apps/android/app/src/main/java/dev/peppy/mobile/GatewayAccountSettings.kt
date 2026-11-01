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
import androidx.compose.runtime.collectAsState
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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.ScreenBody
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale
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
    attachmentStorageOriginSupported: (String?) -> Boolean = { it == "https://peppy.pro" },
    accountLoader: (Context) -> GatewayAccountState? = GatewayAccountHost::load,
    gatewayStatusLoader: (Context) -> GatewayStatus = NativeGateway::status,
    attachmentStorageScopeLoader: (Context) -> GatewayAccountHost.AttachmentStorageScope? = GatewayAccountHost::attachmentStorageScope,
    attachmentStorageLoader: (Context, GatewayAccountHost.AttachmentStorageScope) -> AttachmentStorageLoad? = GatewayAccountHost::loadAttachmentStorage,
    attachmentStorageAcknowledger: (Context, GatewayAccountHost.AttachmentStorageScope) -> Boolean = GatewayAccountHost::acknowledgeAttachmentStorageNotice,
) {
    val context = LocalContext.current
    var state by remember { mutableStateOf<GatewayAccountState?>(null) }
    var gatewayStatus by remember { mutableStateOf<GatewayStatus?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var deleteOpen by remember { mutableStateOf(false) }
    var erase by remember { mutableStateOf("") }
    var deletingVault by remember { mutableStateOf(false) }
    var revokeTarget by remember { mutableStateOf<GatewayAccountDevice?>(null) }
    var devicesOpen by remember { mutableStateOf(false) }
    var storageOpen by remember { mutableStateOf(false) }
    var storageBusy by remember { mutableStateOf(false) }
    var storageAcknowledgementInFlight by remember { mutableStateOf(false) }
    var storageAcknowledgementFailed by remember { mutableStateOf(false) }
    var notificationConsentOpen by remember { mutableStateOf(false) }
    val relayPrefs = remember { context.getSharedPreferences("peppy-relay", 0) }
    var relayEnabled by remember { mutableStateOf(relayPrefs.getBoolean("relay-enabled.v1", false)) }
    var relayOrigin by remember { mutableStateOf(relayPrefs.getString("relay-origin.v1", "") ?: "") }
    var remoteMessagingEnabled by remember { mutableStateOf(RemoteMessagingPreferences.enabled(context)) }
    var wakePreference by remember { mutableStateOf<RelayWakePreference?>(null) }
    var wakeStatus by remember { mutableStateOf(RelayWakeStatus.PENDING) }
    val attachmentStorageController = remember { AttachmentStorageSettingsController() }
    var attachmentStorageScope by remember { mutableStateOf<GatewayAccountHost.AttachmentStorageScope?>(null) }
    var attachmentStorageState by remember { mutableStateOf<AttachmentStorageUiState>(AttachmentStorageUiState.Loading) }
    var relayLoaded by remember { mutableStateOf(false) }
    var relayWritePending by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    DisposableEffect(relayPrefs, context) {
        val listener = android.content.SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            when (key) {
                "hosted-relay-preference.v1" -> scope.launch { wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) } }
                "hosted-relay-status.v1" -> scope.launch { wakeStatus = withContext(Dispatchers.IO) { RelayWakeRuntime.status(context) } }
                "relay-enabled.v1" -> relayEnabled = relayPrefs.getBoolean("relay-enabled.v1", false)
                "relay-origin.v1" -> relayOrigin = relayPrefs.getString("relay-origin.v1", "") ?: ""
            }
        }
        relayPrefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { relayPrefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    suspend fun refreshStorage() {
        if (storageBusy) return
        storageBusy = true
        try {
            val storageScope = withContext(Dispatchers.IO) { attachmentStorageScopeLoader(context) }
            if (storageScope == null) {
                attachmentStorageScope = null
                attachmentStorageController.begin("")
                attachmentStorageState = AttachmentStorageUiState.Unavailable(null)
                return
            }
            val changedScope = attachmentStorageScope?.value != storageScope.value
            attachmentStorageScope = storageScope
            attachmentStorageController.begin(storageScope.value)
            if (changedScope) storageAcknowledgementFailed = false
            attachmentStorageState = attachmentStorageController.state
            val storage = withContext(Dispatchers.IO) { attachmentStorageLoader(context, storageScope) }
            if (storage != null && attachmentStorageController.apply(storageScope.value, storage)) {
                attachmentStorageState = attachmentStorageController.state
            } else if (attachmentStorageScope?.value == storageScope.value) {
                attachmentStorageState = AttachmentStorageUiState.Unavailable(null)
            }
        } finally {
            storageBusy = false
        }
    }
    suspend fun acknowledgeStorage(visibleScope: String) {
        val binding = attachmentStorageScope ?: return
        try {
            val acknowledged = withContext(Dispatchers.IO) { attachmentStorageAcknowledger(context, binding) }
            if (attachmentStorageController.acknowledgeResult(visibleScope, acknowledged)) {
                storageAcknowledgementFailed = false
                refreshStorage()
            } else if (attachmentStorageScope?.value == visibleScope) {
                storageAcknowledgementFailed = true
            }
        } finally {
            if (attachmentStorageScope?.value == visibleScope) storageAcknowledgementInFlight = false
        }
    }
    fun closeStorageDetail() {
        storageOpen = false
        attachmentStorageScope = null
        attachmentStorageController.begin("")
        attachmentStorageState = AttachmentStorageUiState.Loading
        storageAcknowledgementInFlight = false
        storageAcknowledgementFailed = false
    }
    suspend fun refresh() {
        val loaded = withContext(Dispatchers.IO) { accountLoader(context) }
        gatewayStatus = withContext(Dispatchers.IO) { gatewayStatusLoader(context) }
        state = loaded
        if (state == null) failure = "Could not load devices. Unlock or check the server connection."
    }
    LaunchedEffect(Unit) { refresh() }
    LaunchedEffect(storageOpen) { if (storageOpen) refreshStorage() }
    DisposableEffect(devicesOpen, storageOpen) {
        onDetailChanged(devicesOpen || storageOpen)
        onDispose { onDetailChanged(false) }
    }
    BackHandler(enabled = devicesOpen || storageOpen) { if (storageOpen) closeStorageDetail() else devicesOpen = false }
    LaunchedEffect(Unit) {
        wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) }
        wakeStatus = withContext(Dispatchers.IO) { RelayWakeRuntime.status(context) }
        relayLoaded = true
    }
    // Lock and recovery may change while this screen remains open. This is local state only; the
    // explicit Refresh button remains the only roster/server request.
    LaunchedEffect(Unit) {
        while (true) {
            gatewayStatus = withContext(Dispatchers.IO) { gatewayStatusLoader(context) }
            delay(1_000)
        }
    }
    Column(Modifier.fillMaxWidth().testTag(if (settingsMode) "settings-account-group" else if (devicesOpen) "devices-screen" else "account-tab-screen")) {
        val accountReadOnly by AccountAccessState.readOnly.collectAsState()
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
            if (!settingsMode && !devicesOpen && !storageOpen) {
                HeroBanner(
                    title = stringResource(R.string.peppy_account),
                    body = listOfNotNull(
                        gatewayStatus?.origin,
                        stringResource(R.string.peppy_toggle_android_account_role, account.self.role),
                        if (accountReadOnly) stringResource(R.string.peppy_account_send_paused_status) else null,
                    ).joinToString("\n"),
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
                    if (attachmentStorageOriginSupported(gatewayStatus?.origin)) AccountNavigationRow("attachment-storage-row", stringResource(R.string.peppy_attachment_storage_section_title)) { storageOpen = true }
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
            if (storageOpen && attachmentStorageOriginSupported(gatewayStatus?.origin)) {
                ScreenBody("attachment-storage-body") {
                    TextButton(onClick = ::closeStorageDetail, modifier = Modifier.testTag("attachment-storage-back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.peppy_back)) }
                    AttachmentStorageSettings(
                        state = attachmentStorageState,
                        scope = attachmentStorageScope?.value,
                        busy = storageBusy || storageAcknowledgementInFlight,
                        acknowledgementFailed = storageAcknowledgementFailed,
                        onRefresh = {
                            val visibleScope = attachmentStorageScope?.value
                            if (storageAcknowledgementFailed && visibleScope != null && attachmentStorageController.retryFailedAcknowledgement(visibleScope)) {
                                storageAcknowledgementInFlight = true
                                scope.launch { acknowledgeStorage(visibleScope) }
                            } else {
                                scope.launch { refreshStorage() }
                            }
                        },
                        onAcknowledge = { visibleScope ->
                            if (attachmentStorageController.claimDisclosureRendered(visibleScope)) {
                                storageAcknowledgementInFlight = true
                                scope.launch { acknowledgeStorage(visibleScope) }
                            }
                        },
                        onManage = { origin -> openUri(context, "$origin/account", failureSetter = { failure = it }) },
                        onPrivacy = { origin -> openUri(context, "$origin/privacy", failureSetter = { failure = it }) },
                    )
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
            val hostedRelay = gatewayStatus?.origin?.let { hostedWakeRelayUrl(it) }
            val relayReady = relayLoaded && gatewayStatus != null && !relayWritePending
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
                    Switch(
                        checked = wakePreference.let { it != null && it != RelayWakePreference.PAUSED },
                        enabled = relayReady,
                        onCheckedChange = { enabled ->
                            val next = if (enabled) RelayWakePreference.AUTOMATIC else RelayWakePreference.PAUSED
                            relayWritePending = true
                            scope.launch {
                                try {
                                    withContext(Dispatchers.IO) { RelayWakeRuntime.setPreference(context, next) }
                                    wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) }
                                } finally {
                                    relayWritePending = false
                                }
                            }
                        },
                        modifier = Modifier.testTag("relay-wake-toggle"),
                    )
                }
                Text(stringResource(footer), Modifier.testTag("relay-wake-footer"))
            } else {
                OutlinedTextField(relayOrigin, { relayOrigin = it }, enabled = relayReady, label = { Text("Relay HTTPS origin") }, modifier = Modifier.testTag("relay-origin-field"))
                Row(Modifier.fillMaxWidth()) {
                    Text("Enable wake relay", Modifier.weight(1f))
                    Switch(
                        checked = relayEnabled,
                        enabled = relayReady,
                        onCheckedChange = { enabled ->
                            val origin = if (enabled) CredentialParser.canonicalOrigin(relayOrigin, false) else null
                            if (enabled && origin == null) {
                                failure = "Enter a canonical HTTPS relay origin."
                            } else {
                                relayWritePending = true
                                scope.launch {
                                    try {
                                        withContext(Dispatchers.IO) { RelayWakeRuntime.setEnabled(context, origin, enabled) }
                                        relayEnabled = withContext(Dispatchers.IO) { relayPrefs.getBoolean("relay-enabled.v1", false) }
                                    } finally {
                                        relayWritePending = false
                                    }
                                }
                            }
                        },
                        modifier = Modifier.testTag("relay-enable-toggle"),
                    )
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
        onDismissRequest = { if (!deletingVault) { erase = ""; deleteOpen = false } }, title = { Text("Type ERASE to confirm") },
        text = { OutlinedTextField(erase, { erase = it }, enabled = !deletingVault, label = { Text("Type ERASE") }, modifier = Modifier.testTag("vault-delete-confirm-field")) },
        confirmButton = { Button(onClick = { if (deletingVault) return@Button; deletingVault = true; scope.launch { try { val ok = withContext(Dispatchers.IO) { GatewayAccountHost.deleteVault(context) }; if (!ok) failure = "Could not reach the server. The vault was not deleted." else onChanged() } finally { deletingVault = false; erase = ""; deleteOpen = false } } }, enabled = erase == "ERASE" && !deletingVault, modifier = Modifier.testTag("vault-delete-final-button")) { Text("Delete vault") } },
        dismissButton = { OutlinedButton(onClick = { erase = ""; deleteOpen = false }, enabled = !deletingVault) { Text("Keep vault") } },
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

@Composable
internal fun AttachmentStorageSettings(
    state: AttachmentStorageUiState,
    scope: String?,
    busy: Boolean,
    acknowledgementFailed: Boolean,
    onRefresh: () -> Unit,
    onAcknowledge: (String) -> Unit,
    onManage: (String) -> Unit,
    onPrivacy: (String) -> Unit,
) {
    when (state) {
        AttachmentStorageUiState.Loading -> Section("attachment-storage-section", stringResource(R.string.peppy_attachment_storage_section_title)) {
            Text(stringResource(R.string.peppy_toggle_android_account_loading), Modifier.testTag("attachment-storage-loading"))
            TextButton(onClick = onRefresh, enabled = !busy, modifier = Modifier.testTag("attachment-storage-refresh")) { Text(stringResource(R.string.peppy_attachment_storage_reload_cta)) }
        }
        AttachmentStorageUiState.Unsupported -> Section("attachment-storage-section", stringResource(R.string.peppy_attachment_storage_section_title)) {
            Text(stringResource(R.string.peppy_attachment_storage_unavailable), Modifier.testTag("attachment-storage-unavailable"))
            TextButton(onClick = onRefresh, enabled = !busy, modifier = Modifier.testTag("attachment-storage-refresh")) { Text(stringResource(R.string.peppy_attachment_storage_reload_cta)) }
        }
        is AttachmentStorageUiState.Unavailable -> Section("attachment-storage-section", stringResource(R.string.peppy_attachment_storage_section_title)) {
            Text(stringResource(R.string.peppy_attachment_storage_unavailable), Modifier.testTag("attachment-storage-unavailable"))
            state.lastKnown?.let { stale ->
                Text(stringResource(R.string.peppy_attachment_storage_stale_usage), Modifier.testTag("attachment-storage-stale-label"))
                StorageValues(stale, "attachment-storage-stale-values")
            }
            TextButton(onClick = onRefresh, enabled = !busy, modifier = Modifier.testTag("attachment-storage-refresh")) { Text(stringResource(R.string.peppy_attachment_storage_reload_cta)) }
        }
        is AttachmentStorageUiState.Available -> {
            val status = state.status
            Section("attachment-storage-section", stringResource(R.string.peppy_attachment_storage_section_title)) {
                StorageValues(status, "attachment-storage-values")
                Column(Modifier.testTag("attachment-storage-disclosure")) {
                    Text(stringResource(R.string.peppy_attachment_storage_notice))
                    Text(stringResource(R.string.peppy_attachment_storage_protected))
                    Text(stringResource(R.string.peppy_attachment_storage_device_copies))
                    if (status.notice.state == AttachmentStorageNoticeState.NOT_PRESENTED || status.notice.state == AttachmentStorageNoticeState.GRACE_PERIOD) {
                        Text(stringResource(R.string.peppy_attachment_storage_grace))
                        status.notice.activationAtSeconds?.let { seconds ->
                            val announcement = "${stringResource(R.string.peppy_attachment_storage_activation_label)} ${storageDate(seconds)}"
                            Text(announcement, Modifier.semantics { contentDescription = announcement }.testTag("attachment-storage-activation"))
                        }
                    }
                    Text(stringResource(R.string.peppy_attachment_storage_purchase))
                    scope?.let { visibleScope ->
                        if (status.notice.state == AttachmentStorageNoticeState.NOT_PRESENTED && status.noticeCopySupported) {
                            LaunchedEffect(visibleScope, status.notice.policyVersion) { onAcknowledge(visibleScope) }
                        }
                    }
                }
                when {
                    status.cleanup == AttachmentStorageCleanup.DISABLED || status.unavailableReason == AttachmentStorageUnavailableReason.CLEANUP_DISABLED -> Text(stringResource(R.string.peppy_attachment_storage_disabled), Modifier.testTag("attachment-storage-disabled"))
                    status.cleanup == AttachmentStorageCleanup.PENDING -> Text(stringResource(R.string.peppy_attachment_storage_cleanup_pending), Modifier.testTag("attachment-storage-pending"))
                    status.notice.state == AttachmentStorageNoticeState.GRACE_PERIOD && status.usedBytes >= status.limitBytes -> Text(stringResource(R.string.peppy_attachment_storage_grace_full), Modifier.testTag("attachment-storage-grace-full"))
                    status.notice.state == AttachmentStorageNoticeState.GRACE_PERIOD -> Text(stringResource(R.string.peppy_attachment_storage_grace_paused), Modifier.testTag("attachment-storage-grace"))
                    status.unavailableReason == AttachmentStorageUnavailableReason.DEVICE_NOT_READY -> Text(stringResource(R.string.peppy_attachment_storage_readiness), Modifier.testTag("attachment-storage-readiness"))
                }
                if (status.unavailableReason == AttachmentStorageUnavailableReason.READ_ONLY) Text(stringResource(R.string.peppy_attachment_storage_read_only), Modifier.testTag("attachment-storage-read-only"))
                if (status.unavailableReason == AttachmentStorageUnavailableReason.DEVICE_NOT_READY) Text(stringResource(R.string.peppy_attachment_storage_device_update_required), Modifier.testTag("attachment-storage-readiness-caveat"))
                if (status.unavailableReason == AttachmentStorageUnavailableReason.BILLING_MUTATION_PENDING) Text(stringResource(R.string.peppy_attachment_storage_billing_pending), Modifier.testTag("attachment-storage-billing-pending"))
                if (status.unavailableReason == AttachmentStorageUnavailableReason.ENTITLEMENT_UNAVAILABLE || status.unavailableReason == AttachmentStorageUnavailableReason.PROVIDER_RECONCILIATION_PENDING) Text(stringResource(R.string.peppy_attachment_storage_billing_unavailable), Modifier.testTag("attachment-storage-billing-unavailable"))
                Text(stringResource(R.string.peppy_attachment_storage_billing_handoff))
                TextButton(onClick = { onManage("https://peppy.pro") }, modifier = Modifier.testTag("attachment-storage-manage")) { Text(stringResource(R.string.peppy_attachment_storage_manage_cta)) }
                TextButton(onClick = { onPrivacy("https://peppy.pro") }, modifier = Modifier.testTag("attachment-storage-privacy")) { Text(stringResource(R.string.peppy_privacy_policy)) }
                if (acknowledgementFailed) Text(stringResource(R.string.peppy_attachment_storage_notice_save_failed), Modifier.testTag("attachment-storage-ack-failed"))
                TextButton(onClick = onRefresh, enabled = !busy, modifier = Modifier.testTag("attachment-storage-refresh")) { Text(stringResource(R.string.peppy_attachment_storage_reload_cta)) }
            }
        }
    }
}

@Composable
private fun StorageValues(status: AttachmentStorageSnapshot, tag: String) {
    Column(Modifier.testTag(tag)) {
        StatusRow("$tag-used", stringResource(R.string.peppy_attachment_storage_usage), storageBytes(status.usedBytes))
        StatusRow("$tag-limit", stringResource(R.string.peppy_attachment_storage_limit), storageBytes(status.limitBytes))
        StatusRow("$tag-base", stringResource(R.string.peppy_attachment_storage_base_included), storageBytes(status.baseBytes))
    }
}

private fun storageBytes(bytes: Long) = when {
    bytes < 1_000_000L -> "$bytes B"
    bytes < 1_000_000_000L -> "%.1f MB".format(Locale.getDefault(), bytes / 1_000_000.0)
    else -> "%.1f GB".format(Locale.getDefault(), bytes / 1_000_000_000.0)
}
private fun storageDate(seconds: Long) = DateTimeFormatter.ofLocalizedDate(FormatStyle.LONG).withLocale(Locale.getDefault()).withZone(ZoneId.systemDefault()).format(Instant.ofEpochSecond(seconds))

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
