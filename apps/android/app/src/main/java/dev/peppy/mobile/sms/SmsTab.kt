package dev.peppy.mobile.sms

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.R
import dev.peppy.mobile.SMS_PERMISSIONS
import dev.peppy.mobile.Section
import dev.peppy.mobile.account.AccountAccessState
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.ServerReachability
import dev.peppy.mobile.gateway.ServerReachabilityState
import dev.peppy.mobile.granted
import dev.peppy.mobile.mms.MmsSettings
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.advanced.AdvancedFeatureState
import dev.peppy.mobile.advanced.readAdvancedAccess
import dev.peppy.mobile.advanced.readAdvancedFeatureState
import dev.peppy.mobile.advanced.markAdvancedFeaturesSeen
import dev.peppy.mobile.advanced.showNewFeatureHint
import dev.peppy.mobile.restrictedsetup.restrictedSetupAvailable
import dev.peppy.mobile.restrictedsetup.RestrictedSetupEntry
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.phoneread.PhoneReadSettings
import dev.peppy.mobile.syncMessage
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.DetailsNavigationRow
import dev.peppy.mobile.ui.DetailsPageHeader
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.theme.PeppyTokens
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** The effectful SMS policy sequence used by the consent flow. */
internal class SmsCapturePolicyAction(
    private val writeAndRead: suspend (Boolean) -> Boolean,
    private val onActivated: () -> Unit,
) {
    suspend fun prepareFreshReadGrant(readGranted: Boolean, requestAccess: () -> Unit): Boolean {
        if (!readGranted && !writeAndRead(false)) return false
        requestAccess()
        return true
    }

    suspend fun enableAfterGrant(): Boolean {
        if (!writeAndRead(true)) return false
        onActivated()
        return true
    }
}

@Composable
internal fun SmsTab(
    context: Context,
    refresh: Int,
    databaseOpen: Boolean,
    onChanged: () -> Unit,
    gatewayStatus: GatewayStatus? = null,
    onDetailsChanged: (Boolean) -> Unit = {},
) {
    val scope = rememberCoroutineScope()
    var policy by remember(refresh) { mutableStateOf<Boolean?>(null) }
    var consent by remember { mutableStateOf(ConsentGatedToggle()) }
    var offConfirmation by remember { mutableStateOf(false) }
    var writeBusy by remember { mutableStateOf(false) }
    var resultHint by remember { mutableStateOf<String?>(null) }
    var permissionDenied by remember { mutableStateOf(false) }
    var detailsOpen by remember { mutableStateOf(false) }
    var linkDetailsOpen by remember { mutableStateOf(false) }
    var linkNotice by rememberSaveable { mutableStateOf<String?>(null) }
    var linkDismissedSeq by rememberSaveable { mutableStateOf<Long?>(null) }
    var linkNotificationAsked by rememberSaveable { mutableStateOf(false) }
    var history by remember { mutableStateOf(SmsHistoryState.WAITING_FOR_VAULT) }
    var advancedFeatures by remember { mutableStateOf<AdvancedFeatureState?>(null) }
    val accountReadOnly by AccountAccessState.readOnly.collectAsState()
    val deniedHint = stringResource(R.string.peppy_toggle_android_sms_denied_hint)
    val guardFailedHint = stringResource(R.string.peppy_toggle_android_sms_guard_failed)
    val allGranted = SMS_PERMISSIONS.all { granted(context, it) }
    val readGranted = granted(context, Manifest.permission.READ_SMS)
    val reachability by ServerReachabilityState.state.collectAsState()
    val health = gatewayStatus?.let {
        syncHealth(
            it,
            granted(context, Manifest.permission.RECEIVE_SMS),
            granted(context, Manifest.permission.SEND_SMS),
            readGranted,
            accountReadOnly,
            policy == true,
            history,
            reachability,
        )
    }
    val syncDetailsRequested by SyncDetailsRequests.state.collectAsState()
    LaunchedEffect(syncDetailsRequested) {
        if (syncDetailsRequested) {
            SyncDetailsRequests.consume()
            detailsOpen = true
        }
    }
    val policyAction = remember(context) {
        SmsCapturePolicyAction(
            writeAndRead = { enabled ->
                withContext(Dispatchers.IO) {
                    GatewayPolicyHost(context).smsCaptureEnabled = enabled
                    GatewayPolicyHost(context).smsCaptureEnabled == enabled
                }
            },
            onActivated = {
                SmsCaptureLifecycle.onPermissionOrForeground(context)
                GatewayWork.enqueue(context)
            },
        )
    }
    fun applyGrantedAccess() {
        writeBusy = true
        scope.launch {
            try {
                val enabled = policyAction.enableAfterGrant()
                policy = enabled
                if (enabled) onChanged() else resultHint = guardFailedHint
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                resultHint = guardFailedHint
            } finally {
                writeBusy = false
                consent = consent.cancel()
            }
        }
    }
    fun consumePermissionResult(allPermissionsGranted: Boolean) {
        val awaitingAccess = consent.phase == ConsentPhase.AWAITING_ACCESS
        if (!awaitingAccess) return
        permissionDenied = !allPermissionsGranted
        SmsReadPermissionRecovery.recordResult(context, granted(context, Manifest.permission.READ_SMS))
        val transition = consent.accessResult(allPermissionsGranted)
        consent = transition.state
        if (transition.effect == ConsentEffect.APPLY) applyGrantedAccess()
        else {
            resultHint = deniedHint
        }
    }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
        consumePermissionResult(SMS_PERMISSIONS.all { grants[it] == true || granted(context, it) })
    }
    val settingsLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        consumePermissionResult(SMS_PERMISSIONS.all { granted(context, it) })
    }
    LaunchedEffect(refresh) {
        policy = withContext(Dispatchers.IO) { GatewayPolicyHost(context).smsCaptureEnabled }
        history = withContext(Dispatchers.IO) {
            val fallback = smsHistoryState(
                readGranted = smsReadGranted(context),
                capturePermitted = GatewayPolicyHost(context).permitsSmsCapture(),
                databaseOpen = databaseOpen,
            )
            readSmsHistoryState(context, fallback)
        }
        advancedFeatures = withContext(Dispatchers.IO) { readAdvancedFeatureState(context) }
    }
    DisposableEffect(detailsOpen, linkDetailsOpen) {
        onDetailsChanged(detailsOpen || linkDetailsOpen)
        onDispose {
            consent = consent.cancel()
            onDetailsChanged(false)
        }
    }
    LaunchedEffect(detailsOpen) {
        if (!detailsOpen) {
            advancedFeatures = withContext(Dispatchers.IO) { readAdvancedFeatureState(context) }
        }
    }
    fun beginEnable() {
        resultHint = null
        consent = consent.requestEnable()
    }
    fun applyOrRequest() {
        val transition = consent.agree(allGranted)
        consent = transition.state
        if (transition.effect == ConsentEffect.REQUEST_ACCESS) {
            writeBusy = true
            scope.launch {
                try {
                    val requested = policyAction.prepareFreshReadGrant(readGranted) {
                        val activity = context.contextActivity()
                        val permanentlyDenied = permissionDenied && SMS_PERMISSIONS.any {
                            !granted(context, it) && activity?.shouldShowRequestPermissionRationale(it) == false
                        }
                        if (permanentlyDenied || SmsReadPermissionRecovery.needsSettingsRecovery(context, activity?.shouldShowRequestPermissionRationale(Manifest.permission.READ_SMS) == true)) {
                            settingsLauncher.launch(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).setData(android.net.Uri.parse("package:${context.packageName}")))
                        } else launcher.launch(SMS_PERMISSIONS)
                    }
                    if (!requested) {
                        consent = consent.cancel()
                        resultHint = guardFailedHint
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    consent = consent.cancel()
                    resultHint = guardFailedHint
                } finally {
                    writeBusy = false
                }
            }
        }
        if (transition.effect == ConsentEffect.APPLY) {
            applyGrantedAccess()
        }
    }

    if (detailsOpen) {
        dev.peppy.mobile.ui.ScreenBody("sms-details-screen") {
            DetailsPageHeader(stringResource(R.string.peppy_toggle_android_sync_details), "sms-details") { detailsOpen = false }
            SyncSummary(health)
            gatewayStatus?.let {
                val currentHealth = requireNotNull(health)
                Section("status-section", stringResource(R.string.peppy_toggle_android_this_phone), showDivider = false) {
                    CheckRow("status-unlocked", currentHealth.unlocked, stringResource(if (currentHealth.unlocked) R.string.peppy_toggle_android_check_unlocked else R.string.peppy_toggle_android_check_locked))
                    if (currentHealth.smsSyncOn) {
                        CheckRow(
                            "status-receive",
                            currentHealth.receiveAllowed,
                            stringResource(if (currentHealth.receiveAllowed) R.string.peppy_toggle_android_check_receive_ok else R.string.peppy_toggle_android_check_receive_bad),
                            action = if (currentHealth.receiveAllowed) null else {
                                { TextButton(onClick = ::beginEnable, modifier = Modifier.heightIn(min = 48.dp).testTag("status-receive-allow")) { Text(stringResource(R.string.peppy_toggle_android_allow)) } }
                            },
                        )
                        CheckRow(
                            "status-send",
                            !accountReadOnly && currentHealth.sendAllowed,
                            if (accountReadOnly) stringResource(R.string.peppy_account_send_paused_status) else stringResource(if (currentHealth.sendAllowed) R.string.peppy_toggle_android_check_send_ok else R.string.peppy_toggle_android_check_send_bad),
                            action = if (!accountReadOnly && !currentHealth.sendAllowed) {
                                { TextButton(onClick = ::beginEnable, modifier = Modifier.heightIn(min = 48.dp).testTag("status-send-allow")) { Text(stringResource(R.string.peppy_toggle_android_allow)) } }
                            } else null,
                        )
                    }
                    CheckRow(
                        "sms-gateway-sync-status",
                        currentHealth.connected,
                        when {
                            currentHealth.stopped || currentHealth.softRejected -> syncMessage(it)
                            else -> when (currentHealth.reachability) {
                                ServerReachability.AVAILABLE -> stringResource(R.string.peppy_toggle_android_check_connected)
                                ServerReachability.UNKNOWN -> stringResource(R.string.peppy_toggle_android_sync_checking)
                                ServerReachability.UNAVAILABLE -> stringResource(R.string.peppy_toggle_android_sync_connection_unavailable)
                            }
                        },
                    )
                }
            }
            SmsSettings(context, history, policy == true, ::beginEnable)
            MmsSettings(context, refresh, databaseOpen, onChanged, detailsOnly = true)
            if (restrictedSetupAvailable(context)) AdvancedFeaturesSection(context, refresh, onChanged)
            resultHint?.let { Text(it, Modifier.testTag("sms-action-error")) }
        }
    } else if (linkDetailsOpen) {
        GoogleMessagesLink.Details(
            onBack = { linkDetailsOpen = false },
            onChanged = onChanged,
            notice = linkNotice,
            onNoticeChange = { linkNotice = it },
            dismissedSeq = linkDismissedSeq,
            onDismissedSeqChange = { linkDismissedSeq = it },
            notificationAsked = linkNotificationAsked,
            onNotificationAskedChange = { linkNotificationAsked = it },
        )
    } else Column(Modifier.testTag("sms-tab-screen")) {
        HeroBanner(
            stringResource(R.string.peppy_toggle_android_sms_hero_title),
            stringResource(R.string.peppy_toggle_android_sms_hero_body),
            "sms-hero",
        )
        dev.peppy.mobile.ui.ScreenBody("sms-tab-body") {
        GoogleMessagesLink.Entry(
            onChanged = onChanged,
            notice = linkNotice,
            onNoticeChange = { linkNotice = it },
            onManage = { linkDetailsOpen = true },
            dismissedSeq = linkDismissedSeq,
            onDismissedSeqChange = { linkDismissedSeq = it },
            notificationAsked = linkNotificationAsked,
            onNotificationAskedChange = { linkNotificationAsked = it },
        )
        Column(Modifier.fillMaxWidth().testTag("sms-section"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            val actualCapture = policy == true && readGranted
            val subtitle = when {
                policy == null -> null
                policy == true && !readGranted -> stringResource(R.string.peppy_sms_history_waiting_permission)
                actualCapture && !allGranted -> stringResource(R.string.peppy_toggle_android_sms_partial_hint)
                actualCapture -> stringResource(R.string.peppy_toggle_android_sms_active_hint)
                resultHint != null -> resultHint
                else -> stringResource(R.string.peppy_toggle_android_sms_off_hint)
            }
            SettingToggleRow(stringResource(R.string.peppy_toggle_android_sms_label), actualCapture, policy != null && !writeBusy && consent.phase == ConsentPhase.IDLE, { requested ->
                if (requested) beginEnable() else offConfirmation = true
            }, "sms-sync-row", subtitle)
            if (policy == true && !readGranted) TextButton(
                onClick = { offConfirmation = true },
                modifier = Modifier.testTag("sms-keep-off"),
            ) { Text(stringResource(R.string.peppy_toggle_android_sms_keep_off)) }
            if (policy != null && !allGranted && (permissionDenied || readGranted)) OutlinedButton(
                onClick = {
                    beginEnable()
                },
                modifier = Modifier.testTag("sms-open-settings"),
            ) { Text(stringResource(R.string.peppy_toggle_android_sms_review_access)) }
            if (actualCapture) resultHint?.let { Text(it, Modifier.testTag("sms-action-error")) }
         }
         MmsSettings(context, refresh, databaseOpen, onChanged)
         PhoneReadSettings(context, refresh, onChanged)
         Section("battery-section", stringResource(R.string.peppy_toggle_android_background_sync), showDivider = false) {
             BatteryOptimizationRow(context)
         }
        advancedFeatures?.let { features ->
            if (restrictedSetupAvailable(context) && showNewFeatureHint(features.access.first, features.access.second, features.seen)) {
                TextButton(onClick = { detailsOpen = true }, modifier = Modifier.fillMaxWidth().testTag("sms-advanced-features-summary")) {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(stringResource(R.string.peppy_advanced_features_title))
                        Text(stringResource(R.string.peppy_advanced_features_new_available))
                    }
                }
            }
        }
         DetailsNavigationRow("sms-details-row", stringResource(R.string.peppy_toggle_android_sync_details), if (health?.actionNeeded == true) stringResource(R.string.peppy_toggle_android_sync_needs_attention) else null) { detailsOpen = true }
        }
    }
    if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_sms_consent_title),
        body = stringResource(R.string.peppy_permissions_sms_disclosure),
        tag = "sms-consent-sheet",
        onDismiss = { consent = consent.cancel() },
        onConfirm = ::applyOrRequest,
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = if (allGranted) null else stringResource(R.string.peppy_toggle_android_sms_next),
        disclosureTag = "sms-permissions-disclosure",
    )
    if (offConfirmation) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_sms_off_title),
        body = stringResource(R.string.peppy_sms_capture_disabled_hint),
        tag = "sms-off-confirm-sheet",
        onDismiss = { offConfirmation = false },
        onConfirm = {
            offConfirmation = false
            writeBusy = true
            scope.launch {
                try {
                    policy = withContext(Dispatchers.IO) {
                        GatewayPolicyHost(context).smsCaptureEnabled = false
                        GatewayPolicyHost(context).smsCaptureEnabled
                    }
                    if (policy == false) onChanged() else resultHint = guardFailedHint
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    resultHint = guardFailedHint
                } finally {
                    writeBusy = false
                }
            }
        },
        confirmLabel = stringResource(R.string.peppy_toggle_android_turn_off),
        disclosureTag = "sms-off-confirm-disclosure",
        showPrivacyPolicy = false,
        dismissLabel = stringResource(R.string.peppy_toggle_android_keep_on),
    )
}

@Composable
private fun SyncSummary(health: SyncHealth?) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("sms-sync-summary").semantics(mergeDescendants = true) {},
        shape = RoundedCornerShape(12.dp),
        color = when (health?.summaryState) {
            SyncSummaryState.ATTENTION -> colors.warningSurface
            SyncSummaryState.HEALTHY -> colors.success.copy(alpha = 0.12f)
            else -> MaterialTheme.colorScheme.surfaceVariant
        },
    ) {
        Box(Modifier.padding(12.dp)) {
            if (health == null) {
                Text(stringResource(R.string.peppy_toggle_android_sync_checking))
            } else {
                val problem = health.summaryState == SyncSummaryState.ATTENTION
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val state = health.summaryState
                    Text(
                        if (problem) "⚠" else if (state == SyncSummaryState.HEALTHY) "✓" else "•",
                        modifier = Modifier.clearAndSetSemantics {},
                        color = if (problem) colors.warningText else if (state == SyncSummaryState.HEALTHY) colors.success else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Column {
                        Text(
                            stringResource(
                                when (state) {
                                    SyncSummaryState.ATTENTION -> R.string.peppy_toggle_android_sync_needs_attention
                                    SyncSummaryState.HEALTHY -> R.string.peppy_toggle_android_sync_all_good_title
                                    SyncSummaryState.OFF -> R.string.peppy_toggle_android_sync_off_title
                                    SyncSummaryState.RETRYING -> R.string.peppy_toggle_android_sync_retrying_title
                                    SyncSummaryState.CHECKING -> R.string.peppy_toggle_android_sync_checking
                                    SyncSummaryState.PROGRESS -> R.string.peppy_toggle_android_sync_progress_title
                                    SyncSummaryState.HISTORY_PAUSED -> R.string.peppy_toggle_android_sync_history_paused_title
                                },
                            ),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            if (problem && health.problemCount > 1) stringResource(R.string.peppy_toggle_android_sync_attention_many)
                            else if (problem && !health.unlocked) stringResource(R.string.peppy_toggle_android_sync_attention_locked)
                            else if (problem && health.stopped) stringResource(R.string.peppy_toggle_android_sync_attention_stopped)
                            else if (problem) stringResource(R.string.peppy_toggle_android_sync_attention_access)
                            else stringResource(
                                when (state) {
                                    SyncSummaryState.HEALTHY -> R.string.peppy_toggle_android_sync_all_good_body
                                    SyncSummaryState.OFF -> R.string.peppy_toggle_android_sync_off_body
                                    SyncSummaryState.RETRYING -> R.string.peppy_toggle_android_sync_retrying_body
                                    SyncSummaryState.CHECKING -> R.string.peppy_toggle_android_sync_checking
                                    SyncSummaryState.PROGRESS -> R.string.peppy_toggle_android_sync_progress_body
                                    SyncSummaryState.HISTORY_PAUSED -> R.string.peppy_toggle_android_sync_history_paused_body
                                    SyncSummaryState.ATTENTION -> error("Attention has a presentation")
                                },
                            ),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun CheckRow(
    tag: String,
    ok: Boolean,
    label: String,
    action: (@Composable () -> Unit)? = null,
) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    Row(
        Modifier.fillMaxWidth().heightIn(min = 40.dp).testTag(tag),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(if (ok) "✓" else "⚠", color = if (ok) colors.success else colors.warningText)
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyMedium)
        }
        action?.invoke()
    }
}

@Composable
private fun AdvancedFeaturesSection(context: Context, refresh: Int, onChanged: () -> Unit) {
    var capabilities by remember { mutableStateOf<Pair<RestrictedMessageAccess, RestrictedMessageAccess>?>(null) }
    LaunchedEffect(refresh) {
        capabilities = withContext(Dispatchers.IO) { readAdvancedAccess(context).also { markAdvancedFeaturesSeen(context) } }
    }
    val (restricted, blocked) = capabilities ?: return
    if (restricted == RestrictedMessageAccess.UNSUPPORTED && blocked == RestrictedMessageAccess.UNSUPPORTED) return
    Column(Modifier.testTag("advanced-features-section")) {
        RestrictedSetupEntry(onAccessChanged = onChanged)
    }
}

@Composable
private fun BatteryOptimizationRow(context: Context) {
    var refresh by remember { mutableIntStateOf(0) }
    var explain by remember { mutableStateOf(false) }
    var unavailable by remember { mutableStateOf(false) }
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) refresh++ }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    val allowed = refresh.let { context.getSystemService(PowerManager::class.java)?.isIgnoringBatteryOptimizations(context.packageName) == true }
    fun openList() {
        try { context.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
        catch (_: ActivityNotFoundException) { unavailable = true }
    }
    SettingToggleRow(
        stringResource(R.string.peppy_toggle_android_battery_title),
        allowed,
        true,
        { if (allowed) openList() else explain = true },
        "battery-row",
        stringResource(R.string.peppy_toggle_android_battery_subtitle),
        onClickLabel = stringResource(R.string.peppy_toggle_android_open_settings),
        stateDescription = stringResource(if (allowed) R.string.peppy_toggle_android_battery_allowed else R.string.peppy_toggle_android_battery_delayed),
    )
    if (unavailable) Text(stringResource(R.string.peppy_toggle_android_battery_unavailable), Modifier.testTag("battery-settings-unavailable"))
    if (explain) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_battery_title),
        body = stringResource(R.string.peppy_toggle_android_battery_sheet_body),
        tag = "battery-explain-sheet",
        onDismiss = { explain = false },
        onConfirm = { explain = false; openList() },
        confirmLabel = stringResource(R.string.peppy_toggle_android_open_settings),
        showPrivacyPolicy = false,
    )
}

internal fun Context.contextActivity(): android.app.Activity? {
    var current: Context = this
    while (current is ContextWrapper) {
        if (current is android.app.Activity) return current
        current = current.baseContext
    }
    return current as? android.app.Activity
}
