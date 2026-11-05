package dev.peppy.mobile.sms

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.R
import dev.peppy.mobile.SMS_PERMISSIONS
import dev.peppy.mobile.Section
import dev.peppy.mobile.StatusRow
import dev.peppy.mobile.account.AccountAccessState
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.SyncPhase
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
import dev.peppy.mobile.telephony.SimRoutes
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
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
    var history by remember { mutableStateOf(SmsHistoryState.WAITING_FOR_VAULT) }
    var advancedFeatures by remember { mutableStateOf<AdvancedFeatureState?>(null) }
    val deniedHint = stringResource(R.string.peppy_toggle_android_sms_denied_hint)
    val guardFailedHint = stringResource(R.string.peppy_toggle_android_sms_guard_failed)
    val allGranted = SMS_PERMISSIONS.all { granted(context, it) }
    val readGranted = granted(context, Manifest.permission.READ_SMS)
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
    DisposableEffect(detailsOpen) {
        onDetailsChanged(detailsOpen)
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
        BackHandler { detailsOpen = false }
        dev.peppy.mobile.ui.ScreenBody("sms-details-screen") {
            TextButton(onClick = { detailsOpen = false }, modifier = Modifier.testTag("sms-details-back")) { Text(stringResource(R.string.peppy_back)) }
            gatewayStatus?.let {
                val accountReadOnly by AccountAccessState.readOnly.collectAsState()
                Section("status-section", stringResource(R.string.peppy_toggle_android_phone_status), showDivider = false) {
                    StatusRow("status-credential", stringResource(R.string.peppy_toggle_android_device_credential), stringResource(if (it.enrolled) R.string.peppy_toggle_android_enrolled else R.string.peppy_toggle_android_not_enrolled))
                    StatusRow("status-database", stringResource(R.string.peppy_toggle_android_local_database), stringResource(if (it.databaseOpen) R.string.peppy_toggle_android_database_open else R.string.peppy_toggle_android_database_closed))
                    StatusRow("status-vault-keys", stringResource(R.string.peppy_toggle_android_vault_keys), stringResource(if (it.sharedKeysReady) R.string.peppy_toggle_android_keys_unlocked else R.string.peppy_toggle_android_keys_locked))
                    StatusRow("status-receive", stringResource(R.string.peppy_toggle_android_receive_sms), stringResource(if (granted(context, Manifest.permission.RECEIVE_SMS)) R.string.peppy_sms_history_allowed else R.string.peppy_sms_history_not_allowed))
                    StatusRow("status-send", stringResource(R.string.peppy_toggle_android_send_sms), if (accountReadOnly) stringResource(R.string.peppy_account_send_paused_status) else stringResource(if (granted(context, Manifest.permission.SEND_SMS)) R.string.peppy_sms_history_allowed else R.string.peppy_sms_history_not_allowed))
                    StatusRow("status-sim", stringResource(R.string.peppy_toggle_android_sim_route), stringResource(if (SimRoutes.current().isEmpty()) R.string.peppy_toggle_android_sim_missing else R.string.peppy_toggle_android_sim_default))
                }
                StatusRow(
                    "sms-gateway-sync-status",
                    stringResource(R.string.peppy_remote_messaging_status_label),
                    syncMessage(it),
                )
            }
            if (restrictedSetupAvailable(context)) AdvancedFeaturesSection(context, refresh, onChanged)
            GoogleMessagesLink.Entry(onChanged)
            MmsSettings(context, refresh, databaseOpen, onChanged, detailsOnly = true)
            SmsSettings(context, history)
        }
    } else Column(Modifier.testTag("sms-tab-screen")) {
        HeroBanner(
            stringResource(R.string.peppy_toggle_android_sms_hero_title),
            stringResource(R.string.peppy_toggle_android_sms_hero_body),
            "sms-hero",
        )
        dev.peppy.mobile.ui.ScreenBody("sms-tab-body") {
        Text(stringResource(R.string.peppy_carrier_not_encrypted), Modifier.testTag("sms-carrier-notice"))
        TextButton(onClick = {
            try {
                context.startActivity(Intent(Intent.ACTION_VIEW, android.net.Uri.parse("https://peppy.pro/privacy")))
            } catch (_: ActivityNotFoundException) {
                resultHint = context.getString(R.string.peppy_toggle_android_link_unavailable)
            }
        }, Modifier.testTag("sms-privacy-policy")) { Text(stringResource(R.string.peppy_privacy_policy)) }
        Text(stringResource(R.string.peppy_privacy_policy_scope), Modifier.testTag("sms-privacy-scope"))
        Section("sms-section", stringResource(R.string.peppy_sms)) {
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
        val attention = history == SmsHistoryState.PAUSED || history == SmsHistoryState.UNAVAILABLE || history == SmsHistoryState.PENDING ||
            gatewayStatus?.let { !it.databaseOpen || !it.sharedKeysReady || it.syncPhase != SyncPhase.LIVE || it.outboxRejection != null || it.softRejection != null } == true
        if (attention) Text(stringResource(smsHistoryLabel(history)), Modifier.testTag("sms-details-attention"))
        gatewayStatus?.takeIf { attention }?.let { Text(syncMessage(it), Modifier.testTag("sms-gateway-attention")) }
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
        OutlinedButton(onClick = { detailsOpen = true }, modifier = Modifier.testTag("sms-details-row")) { Text(stringResource(R.string.peppy_toggle_android_sync_details)) }
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
