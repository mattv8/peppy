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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Notification access and capture choices stay separate from SMS and vault-unlock status. */
@Composable
internal fun NotificationMirroringSettings(refresh: Int, onChanged: () -> Unit, onOpenAppFilters: () -> Unit) {
    val context = LocalContext.current
    var accessGranted by remember { mutableStateOf(false) }
    var enabled by remember { mutableStateOf(false) }
    var wifiOnly by remember { mutableStateOf(false) }
    var skipSilent by remember { mutableStateOf(false) }
    var filterSummary by remember { mutableStateOf<NotificationAppFilterSummaryState>(NotificationAppFilterSummaryState.Loading) }
    var testResult by remember { mutableStateOf<NotificationTestCaptureResult?>(null) }
    val scope = rememberCoroutineScope()
    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        onChanged()
    }
    LaunchedEffect(refresh) {
        accessGranted = NotificationMirrorAccess.granted(context)
        enabled = NotificationMirrorPreferences.enabled(context)
        wifiOnly = GatewayPolicyHost(context).mirroringWifiOnly
        skipSilent = GatewayPolicyHost(context).skipSilent
        filterSummary = when (val loaded = NativeNotificationAppFilterActions(context.applicationContext, includeAppInventory = false).load()) {
            is NotificationAppFilterLoadResult.Ready -> NotificationAppFilterSummaryState.Ready(loaded.mutedPackages)
            NotificationAppFilterLoadResult.Locked -> NotificationAppFilterSummaryState.Locked
            NotificationAppFilterLoadResult.Error -> NotificationAppFilterSummaryState.Error
            NotificationAppFilterLoadResult.Loading -> NotificationAppFilterSummaryState.Loading
        }
    }

    Section("notification-mirroring-section", "Notification mirroring", showDivider = false) {
        Text(
            "Upgrade your other Peppy clients before enabling mirroring. Notification history is retained like messages and shares the vault's 100,000-record recovery limit. Mute noisy apps to limit growth.",
            style = MaterialTheme.typography.bodySmall,
        )
        StatusRow("status-notification-access", "Notification access", if (accessGranted) "Granted" else "Not granted")
        if (!accessGranted) {
            OutlinedButton(
                onClick = { accessLauncher.launch(NotificationMirrorAccess.settingsIntent()) },
                modifier = Modifier.testTag("notification-access-button"),
            ) { Text("Grant notification access") }
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
            Column(Modifier.weight(1f)) {
                Text("Mirror app notifications", style = MaterialTheme.typography.bodyMedium)
                Text("Send app notifications to your desktop. Off by default.", style = MaterialTheme.typography.bodySmall)
            }
            Switch(
                checked = enabled,
                enabled = accessGranted,
                onCheckedChange = { value ->
                    NotificationMirrorPreferences.setEnabled(context, value)
                    enabled = value
                    if (value) { NotificationMirrorService.reconcile(); GatewayWork.enqueue(context) }
                    else NotificationMirrorService.clearPendingCaptures()
                },
                modifier = Modifier.testTag("notification-mirroring-master-switch"),
            )
        }
        if (enabled && accessGranted) {
            Row(
                Modifier.fillMaxWidth().testTag("notification-mirroring-wifi-row"),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Mirror on WiFi only", style = MaterialTheme.typography.bodyMedium)
                    Text("WiFi only uses the active WiFi connection. Ethernet and cellular (even unlimited plans) are not included.", style = MaterialTheme.typography.bodySmall)
                }
                Switch(wifiOnly, { value -> GatewayPolicyHost(context).mirroringWifiOnly = value; wifiOnly = value }, modifier = Modifier.testTag("notification-mirroring-wifi-switch"))
            }
            Row(
                Modifier.fillMaxWidth().testTag("notification-mirroring-skip-silent-row"),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(stringResource(R.string.peppy_skip_silent), Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                Switch(skipSilent, { value -> GatewayPolicyHost(context).skipSilent = value; skipSilent = value }, modifier = Modifier.testTag("notification-mirroring-skip-silent-switch"))
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

/** Launchable apps are an optional inventory; observed apps fill gaps without QUERY_ALL_PACKAGES. */
internal fun notificationApps(context: android.content.Context): List<ObservedNotificationApp> {
    val launchers = context.packageManager.queryIntentActivities(
        Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0,
    ).map { info -> ObservedNotificationApp(info.activityInfo.packageName, info.loadLabel(context.packageManager).toString().take(256)) }
    return (launchers + NotificationMirrorService.observedApps()).associateBy { it.packageName }.values
        .filter { it.packageName != context.packageName }.sortedBy { it.label.lowercase() }
}
