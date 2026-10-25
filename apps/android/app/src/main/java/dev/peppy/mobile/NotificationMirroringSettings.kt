package dev.peppy.mobile

import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.ui.InfoButton
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Notification access and capture choices stay separate from SMS and vault-unlock status. */
@Composable
internal fun NotificationMirroringSettings(refresh: Int, onChanged: () -> Unit, onOpenAppFilters: () -> Unit) {
    val context = LocalContext.current
    var accessGranted by remember { mutableStateOf(false) }
    var enabled by remember { mutableStateOf<Boolean?>(null) }
    var wifiOnly by remember { mutableStateOf<Boolean?>(null) }
    var skipSilent by remember { mutableStateOf<Boolean?>(null) }
    var filterSummary by remember { mutableStateOf<NotificationAppFilterSummaryState>(NotificationAppFilterSummaryState.Loading) }
    var testResult by remember { mutableStateOf<NotificationTestCaptureResult?>(null) }
    val scope = rememberCoroutineScope()
    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        onChanged()
    }
    LaunchedEffect(refresh) {
        accessGranted = NotificationMirrorAccess.granted(context)
        enabled = withContext(Dispatchers.IO) { NotificationMirrorPreferences.enabled(context) }
        wifiOnly = withContext(Dispatchers.IO) { GatewayPolicyHost(context).mirroringWifiOnly }
        skipSilent = withContext(Dispatchers.IO) { GatewayPolicyHost(context).skipSilent }
        filterSummary = withContext(Dispatchers.IO) {
            when (val loaded = NativeNotificationAppFilterActions(context.applicationContext, includeAppInventory = false).load()) {
                is NotificationAppFilterLoadResult.Ready -> NotificationAppFilterSummaryState.Ready(loaded.mutedPackages)
                NotificationAppFilterLoadResult.Locked -> NotificationAppFilterSummaryState.Locked
                NotificationAppFilterLoadResult.Error -> NotificationAppFilterSummaryState.Error
                NotificationAppFilterLoadResult.Loading -> NotificationAppFilterSummaryState.Loading
            }
        }
    }

    Section(
        "notification-mirroring-section",
        "Notification mirroring",
        showDivider = false,
        helpBody = stringResource(R.string.peppy_help_android_notification_mirroring),
    ) {
        NotificationMirroringAccessControls(
            accessGranted = accessGranted,
            enabled = enabled == true,
            toggleEnabled = enabled != null,
            onRequestAccess = { accessLauncher.launch(NotificationMirrorAccess.settingsIntent()) },
            onEnabledChanged = { value ->
                // Null disables the control until the write's read-back lands, so taps cannot interleave.
                enabled = null
                scope.launch {
                    val persistedValue = withContext(Dispatchers.IO) {
                        NotificationMirrorPreferences.setEnabled(context, value)
                        NotificationMirrorPreferences.enabled(context)
                    }
                    enabled = persistedValue
                    if (persistedValue) { NotificationMirrorService.reconcile(); GatewayWork.enqueue(context) }
                    else NotificationMirrorService.clearPendingCaptures()
                }
            },
        )
        if (enabled == true && accessGranted) {
            Row(
                Modifier.fillMaxWidth().testTag("notification-mirroring-wifi-row"),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Mirror on WiFi only", style = MaterialTheme.typography.bodyMedium)
                }
                InfoButton(
                    "Mirror on WiFi only",
                    "Mirror on WiFi only",
                    stringResource(R.string.peppy_help_android_notification_wifi),
                    "info-notification-wifi",
                )
                Switch(
                    checked = wifiOnly == true,
                    enabled = wifiOnly != null,
                    onCheckedChange = { value ->
                        wifiOnly = null
                        scope.launch {
                            wifiOnly = withContext(Dispatchers.IO) {
                                GatewayPolicyHost(context).mirroringWifiOnly = value
                                GatewayPolicyHost(context).mirroringWifiOnly
                            }
                        }
                    },
                    modifier = Modifier.testTag("notification-mirroring-wifi-switch"),
                )
            }
            Row(
                Modifier.fillMaxWidth().testTag("notification-mirroring-skip-silent-row"),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(stringResource(R.string.peppy_skip_silent), Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                Switch(
                    checked = skipSilent == true,
                    enabled = skipSilent != null,
                    onCheckedChange = { value ->
                        skipSilent = null
                        scope.launch {
                            skipSilent = withContext(Dispatchers.IO) {
                                GatewayPolicyHost(context).skipSilent = value
                                GatewayPolicyHost(context).skipSilent
                            }
                        }
                    },
                    modifier = Modifier.testTag("notification-mirroring-skip-silent-switch"),
                )
            }
            NotificationAppFiltersNavRow(filterSummary, onOpenAppFilters)
            if (filterSummary is NotificationAppFilterSummaryState.Ready) OutlinedButton(
                onClick = {
                    scope.launch {
                        testResult = withContext(Dispatchers.IO) { NotificationMirrorService.captureTest(context) }
                        if (testResult == NotificationTestCaptureResult.CAPTURED) onChanged()
                    }
                },
                modifier = Modifier.testTag("notification-test-button"),
            ) { Text("Send test notification") }
            when (testResult) {
                NotificationTestCaptureResult.CAPTURED -> Text("Test notification queued for sync.", modifier = Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
                NotificationTestCaptureResult.ALREADY_QUEUED -> Text("A test notification is already queued for sync.", modifier = Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
                NotificationTestCaptureResult.LOCKED -> Text("Unlock sync before sending a test notification.", modifier = Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
                NotificationTestCaptureResult.POLICY_BLOCKED -> Text("Enable notification mirroring and notification access before sending a test notification.", modifier = Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
                NotificationTestCaptureResult.ERROR -> Text("Could not create a test notification. Try again.", modifier = Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
                null -> Unit
            }
        }
    }
}

@Composable
internal fun NotificationMirroringAccessControls(
    accessGranted: Boolean,
    enabled: Boolean,
    toggleEnabled: Boolean = true,
    onRequestAccess: () -> Unit,
    onEnabledChanged: (Boolean) -> Unit,
) {
    Text(
        stringResource(R.string.peppy_notification_mirroring_disclosure),
        Modifier.testTag("notification-mirroring-disclosure"),
        style = MaterialTheme.typography.bodyMedium,
    )
    StatusRow("status-notification-access", "Notification access", if (accessGranted) "Granted" else "Not granted")
    if (!accessGranted) {
        OutlinedButton(onClick = onRequestAccess, modifier = Modifier.testTag("notification-access-button")) { Text("Grant notification access") }
        Text(
            "Peppy needs notification access to mirror app notifications to your desktop. Tap to open Android Settings, then enable Peppy.",
            style = MaterialTheme.typography.bodySmall,
        )
    }
    Row(
        Modifier.fillMaxWidth().testTag("notification-mirroring-master-row"),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Mirror app notifications", Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
        InfoButton(
            "Mirror app notifications",
            "Mirror app notifications",
            stringResource(R.string.peppy_help_android_notification_mirroring_toggle),
            "info-notification-mirroring-toggle",
        )
        Switch(
            checked = enabled,
            enabled = accessGranted && toggleEnabled,
            onCheckedChange = onEnabledChanged,
            modifier = Modifier.testTag("notification-mirroring-master-switch"),
        )
    }
}

/** Launchable apps are an optional inventory; observed apps fill gaps without QUERY_ALL_PACKAGES. */
internal fun notificationApps(context: android.content.Context): List<ObservedNotificationApp> {
    val launchers = context.packageManager.queryIntentActivities(
        Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0,
    ).map { info -> ObservedNotificationApp(info.activityInfo.packageName, info.loadLabel(context.packageManager).toString().take(256)) }
    return (launchers + NotificationMirrorService.observedApps()).associateBy { it.packageName }.values
        .filter { it.packageName != context.packageName }.sortedBy { it.label.lowercase() }
}
