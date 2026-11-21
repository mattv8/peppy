package dev.peppy.mobile.settings

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.os.Build
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.Sync
import androidx.compose.material.icons.filled.WifiTethering
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.GatewayEnqueueCause
import dev.peppy.mobile.R
import dev.peppy.mobile.RelayWakePreference
import dev.peppy.mobile.RelayWakeRuntime
import dev.peppy.mobile.RelayWakeStatus
import dev.peppy.mobile.RemoteMessagingPreferences
import dev.peppy.mobile.RemoteMessagingService
import dev.peppy.mobile.RemoteMessagingStatus
import dev.peppy.mobile.ui.ActionRow
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.InfoSpec
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.SettingsGroup
import dev.peppy.mobile.gateway.SyncPhase
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun SyncSettingsGroup(state: SettingsState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    SettingsGroup(tag = "settings-sync-section", title = stringResource(R.string.peppy_toggle_android_settings_sync), showDivider = false) {
        if (state.liveSyncShown) LiveSyncRow(state)
        if (state.liveSyncRole && !state.notificationsGranted) ActionRow(
            tag = "remote-messaging-notification-permission",
            label = stringResource(R.string.peppy_settings_notifications_off),
            actionLabel = stringResource(R.string.peppy_toggle_android_allow),
            onAction = { state.notificationConsentOpen = true },
            icon = Icons.Default.NotificationsOff,
            info = InfoSpec(
                title = stringResource(R.string.peppy_settings_notifications_off),
                body = stringResource(R.string.peppy_remote_messaging_notifications_disabled),
                tag = "info-remote-messaging-notifications",
            ),
            warning = true,
        )
        if (state.backgroundVisible) WakeUpdatesRow(state, context, scope)
        if (state.accountOffline) Text(
            stringResource(R.string.peppy_toggle_android_account_offline_body),
            Modifier.padding(vertical = 8.dp).testTag("settings-account-offline"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    if (state.notificationConsentOpen) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_settings_notifications_title),
        body = stringResource(R.string.peppy_toggle_android_settings_notifications_rationale),
        tag = "remote-messaging-notification-consent",
        onDismiss = { state.notificationConsentOpen = false },
        onConfirm = {
            state.notificationConsentOpen = false
            if (Build.VERSION.SDK_INT >= 33) context.findActivity()?.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        },
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
    )
}

@Composable
private fun LiveSyncRow(state: SettingsState) {
    val context = LocalContext.current
    val label = stringResource(R.string.peppy_remote_messaging_label)
    val subtitle = when {
        !state.accountLoaded -> stringResource(R.string.peppy_toggle_android_account_loading)
        state.gatewayStatus?.syncPhase == SyncPhase.RECOVERY_REQUIRED -> stringResource(R.string.peppy_remote_messaging_recovery_required)
        !state.remoteMessagingEnabled -> stringResource(R.string.peppy_settings_off)
        else -> stringResource(state.liveSyncStatus.textRes())
    }
    SettingToggleRow(
        title = label,
        checked = state.remoteMessagingEnabled,
        enabled = state.liveSyncEnabled,
        onCheckedChange = { enabled ->
            state.remoteMessagingEnabled = enabled
            RemoteMessagingPreferences.setEnabled(context, enabled)
            if (enabled) RemoteMessagingService.resumeOrEnqueue(context, GatewayEnqueueCause.SETTINGS) else RemoteMessagingService.stop(context)
        },
        tag = "remote-messaging-toggle",
        subtitle = subtitle,
        titleTrailing = {
            InfoButton(
                topic = label,
                title = label,
                body = stringResource(R.string.peppy_remote_messaging_hint),
                tag = "info-remote-messaging",
            )
        },
        icon = Icons.Default.Sync,
    )
}

@Composable
private fun WakeUpdatesRow(
    state: SettingsState,
    context: Context,
    scope: CoroutineScope,
) {
    val title = stringResource(R.string.peppy_wake_section_title)
    val footer = stringResource(state.wakeStatus.footerRes())
    SettingToggleRow(
        title = title,
        checked = state.wakePreference.let { it != null && it != RelayWakePreference.PAUSED },
        enabled = state.relayReady,
        onCheckedChange = { enabled ->
            val next = if (enabled) RelayWakePreference.AUTOMATIC else RelayWakePreference.PAUSED
            state.relayWritePending = true
            scope.launch {
                try {
                    withContext(Dispatchers.IO) { RelayWakeRuntime.setPreference(context, next) }
                    state.wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) }
                } finally {
                    state.relayWritePending = false
                }
            }
        },
        tag = "relay-wake-toggle",
        subtitle = stringResource(state.wakeStatus.statusRes()),
        titleTrailing = {
            InfoButton(topic = title, title = title, body = footer, tag = "info-relay-wake")
        },
        icon = Icons.Default.WifiTethering,
    )
}

private fun RemoteMessagingStatus.textRes(): Int = when (this) {
    RemoteMessagingStatus.CONNECTED -> R.string.peppy_remote_messaging_status_connected
    RemoteMessagingStatus.RECONNECTING -> R.string.peppy_remote_messaging_status_reconnecting
    RemoteMessagingStatus.LOCKED -> R.string.peppy_remote_messaging_status_locked
}

private fun RelayWakeStatus.statusRes(): Int = when (this) {
    RelayWakeStatus.REGISTERED -> R.string.peppy_wake_status_registered
    RelayWakeStatus.RETRYING -> R.string.peppy_wake_status_retrying
    RelayWakeStatus.PAUSED -> R.string.peppy_wake_status_paused
    RelayWakeStatus.UNAVAILABLE -> R.string.peppy_wake_status_unavailable
    RelayWakeStatus.PENDING -> R.string.peppy_wake_status_pending
    RelayWakeStatus.MISSING_APP_SUPPORT -> R.string.peppy_wake_status_unavailable
    RelayWakeStatus.CLEANUP_PENDING -> R.string.peppy_wake_status_paused
}

private fun RelayWakeStatus.footerRes(): Int = when (this) {
    RelayWakeStatus.REGISTERED -> R.string.peppy_wake_footer_registered
    RelayWakeStatus.RETRYING -> R.string.peppy_wake_footer_retrying
    RelayWakeStatus.PAUSED -> R.string.peppy_wake_footer_paused
    RelayWakeStatus.UNAVAILABLE -> R.string.peppy_wake_footer_unavailable_server
    RelayWakeStatus.PENDING -> R.string.peppy_wake_footer_pending
    RelayWakeStatus.MISSING_APP_SUPPORT -> R.string.peppy_wake_footer_missing_app_support
    RelayWakeStatus.CLEANUP_PENDING -> R.string.peppy_wake_cleanup_pending
}

private fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
