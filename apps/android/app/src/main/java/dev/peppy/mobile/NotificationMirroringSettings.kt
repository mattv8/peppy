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
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
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
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Notification access and capture choices stay separate from SMS and vault-unlock status. */
@Composable
internal fun NotificationMirroringSettings(refresh: Int, onChanged: () -> Unit, onOpenAppFilters: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var accessGranted by remember { mutableStateOf(false) }
    var enabled by remember { mutableStateOf<Boolean?>(null) }
    var wifiOnly by remember { mutableStateOf<Boolean?>(null) }
    var skipSilent by remember { mutableStateOf<Boolean?>(null) }
    var filterSummary by remember { mutableStateOf<NotificationAppFilterSummaryState>(NotificationAppFilterSummaryState.Loading) }
    var testResult by remember { mutableStateOf<NotificationTestCaptureResult?>(null) }
    var consent by remember { mutableStateOf(ConsentGatedToggle()) }

    fun applyEnabled(value: Boolean) {
        enabled = null
        scope.launch {
            val persistedValue = withContext(Dispatchers.IO) {
                if (value && !NotificationMirrorAccess.granted(context)) {
                    return@withContext NotificationMirrorPreferences.enabled(context)
                }
                NotificationMirrorPreferences.setEnabled(context, value)
                NotificationMirrorPreferences.enabled(context)
            }
            enabled = persistedValue
            if (persistedValue) {
                NotificationMirrorService.reconcile()
                GatewayWork.enqueue(context)
            } else {
                NotificationMirrorService.clearPendingCaptures()
            }
            consent = consent.cancel()
            onChanged()
        }
    }

    fun consumeAccessResult(granted: Boolean) {
        val transition = consent.accessResult(granted)
        consent = transition.state
        if (transition.effect == ConsentEffect.APPLY) applyEnabled(true)
    }

    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        scope.launch {
            accessGranted = withContext(Dispatchers.IO) { NotificationMirrorAccess.granted(context) }
            consumeAccessResult(accessGranted)
            onChanged()
        }
    }
    DisposableEffect(Unit) { onDispose { consent = consent.cancel() } }

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

    Column(Modifier.testTag("mirroring-tab-screen")) {
        HeroBanner(
            title = stringResource(R.string.peppy_toggle_android_mirroring_hero_title),
            body = stringResource(R.string.peppy_toggle_android_mirroring_hero_body),
            tag = "mirroring-hero",
        )
        ScreenBody("mirroring-screen-body") {
        Section("mirroring-section", "Notification mirroring", showDivider = false, helpBody = stringResource(R.string.peppy_help_android_notification_mirroring)) {
            val effectiveEnabled = enabled == true && accessGranted
            SettingToggleRow(
                title = stringResource(R.string.peppy_toggle_android_mirroring_label),
                checked = enabled == true,
                enabled = enabled != null && consent.phase == ConsentPhase.IDLE,
                onCheckedChange = { requested ->
                    if (requested) consent = consent.requestEnable() else applyEnabled(false)
                },
                tag = "notification-mirroring-master-row",
                subtitle = when {
                    enabled == true && !accessGranted -> stringResource(R.string.peppy_toggle_android_mirroring_paused_hint)
                    !accessGranted -> stringResource(R.string.peppy_toggle_android_mirroring_access_hint)
                    else -> null
                },
            )
            SettingToggleRow(
                    title = stringResource(R.string.peppy_wifi_only),
                    checked = wifiOnly == true,
                    enabled = effectiveEnabled && wifiOnly != null,
                    onCheckedChange = { value ->
                        wifiOnly = null
                        scope.launch { wifiOnly = withContext(Dispatchers.IO) { GatewayPolicyHost(context).mirroringWifiOnly = value; GatewayPolicyHost(context).mirroringWifiOnly } }
                    },
                    tag = "notification-mirroring-wifi-row",
                    subtitle = stringResource(R.string.peppy_toggle_android_mirroring_wifi_hint),
            )
            SettingToggleRow(
                    title = stringResource(R.string.peppy_skip_silent),
                    checked = skipSilent == true,
                    enabled = effectiveEnabled && skipSilent != null,
                    onCheckedChange = { value ->
                        skipSilent = null
                        scope.launch { skipSilent = withContext(Dispatchers.IO) { GatewayPolicyHost(context).skipSilent = value; GatewayPolicyHost(context).skipSilent } }
                    },
                    tag = "notification-mirroring-skip-silent-row",
            )
            if (effectiveEnabled) NotificationAppFiltersNavRow(filterSummary, onOpenAppFilters)
            else Text(stringResource(R.string.peppy_toggle_android_mirroring_app_filters_disabled), Modifier.testTag("app-filters-disabled-row"), style = MaterialTheme.typography.bodySmall)
            if (effectiveEnabled) {
                if (filterSummary is NotificationAppFilterSummaryState.Ready) OutlinedButton(
                    onClick = { scope.launch { testResult = withContext(Dispatchers.IO) { NotificationMirrorService.captureTest(context) }; if (testResult == NotificationTestCaptureResult.CAPTURED) onChanged() } },
                    modifier = Modifier.testTag("notification-test-button"),
                ) { Text("Send test notification") }
                NotificationTestResult(testResult)
            }
        }
        }
    }

    if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_mirroring_consent_title),
        body = stringResource(R.string.peppy_notification_mirroring_disclosure),
        tag = "mirroring-consent-sheet",
        disclosureTag = "notification-mirroring-disclosure",
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = if (accessGranted) null else stringResource(R.string.peppy_toggle_android_mirroring_next_access),
        onDismiss = { consent = consent.cancel() },
        onConfirm = {
            val transition = consent.agree(accessGranted)
            consent = transition.state
            when (transition.effect) {
                ConsentEffect.APPLY -> applyEnabled(true)
                ConsentEffect.REQUEST_ACCESS -> accessLauncher.launch(NotificationMirrorAccess.settingsIntent())
                ConsentEffect.NONE -> Unit
            }
        },
    )
}

@Composable
private fun NotificationTestResult(result: NotificationTestCaptureResult?) {
    val message = when (result) {
        NotificationTestCaptureResult.CAPTURED -> "Test notification queued for sync."
        NotificationTestCaptureResult.ALREADY_QUEUED -> "A test notification is already queued for sync."
        NotificationTestCaptureResult.LOCKED -> "Unlock sync before sending a test notification."
        NotificationTestCaptureResult.POLICY_BLOCKED -> "Enable notification mirroring and notification access before sending a test notification."
        NotificationTestCaptureResult.ERROR -> "Could not create a test notification. Try again."
        null -> return
    }
    Text(message, Modifier.testTag("notification-test-result"), style = MaterialTheme.typography.bodySmall)
}

/** Launchable apps are an optional inventory; observed apps fill gaps without QUERY_ALL_PACKAGES. */
internal fun notificationApps(context: android.content.Context): List<ObservedNotificationApp> {
    val launchers = context.packageManager.queryIntentActivities(
        Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0,
    ).map { info -> ObservedNotificationApp(info.activityInfo.packageName, info.loadLabel(context.packageManager).toString().take(256)) }
    return (launchers + NotificationMirrorService.observedApps()).associateBy { it.packageName }.values
        .filter { it.packageName != context.packageName }.sortedBy { it.label.lowercase() }
}
