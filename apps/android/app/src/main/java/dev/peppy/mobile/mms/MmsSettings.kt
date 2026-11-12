package dev.peppy.mobile.mms

import android.Manifest
import android.content.Context
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.MMS_PERMISSIONS
import dev.peppy.mobile.MmsCapture
import dev.peppy.mobile.MmsCaptureWork
import dev.peppy.mobile.R
import dev.peppy.mobile.Section
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.granted
import dev.peppy.mobile.sms.SmsCapturePolicyAction
import dev.peppy.mobile.sms.contextActivity
import dev.peppy.mobile.sms.smsReadGranted
import dev.peppy.mobile.restrictedsetup.restrictedSetupAvailable
import dev.peppy.mobile.telephony.OwnNumberSettings
import dev.peppy.mobile.telephony.SimRoutes
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.DetailsStatusRow as StatusRow
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPause
import uniffi.peppy_mobile_bindings.NativeMmsBackfillStatus

internal data class MmsHealth(
    val pending: Int = 0,
    val blocked: Int = 0,
    val unavailable: Int = 0,
    val transferReasons: List<String> = emptyList(),
    val databaseUnavailable: Boolean = false,
    val backfillStatus: MmsBackfillStatus? = null,
    val quarantinedCount: Long = 0L,
)

internal sealed interface MmsBackfillStatus {
    data object PausedPhoneStorage : MmsBackfillStatus
    data object OlderPausedHistoryLimit : MmsBackfillStatus
    data object PausedHistoryLimit : MmsBackfillStatus
    data object OlderPausedServerStorage : MmsBackfillStatus
    data object GettingReady : MmsBackfillStatus
    data object CopyingOlder : MmsBackfillStatus
    data class Copying(val count: ULong) : MmsBackfillStatus
    data object UpToDate : MmsBackfillStatus
}

internal enum class MmsPresentationStatus {
    OFF, ACCESS_NEEDED, LOCKED, GETTING_READY, COPYING_OLDER, COPYING, UP_TO_DATE, MOSTLY_UP_TO_DATE,
    PHONE_STORAGE_PAUSED, OLDER_HISTORY_PAUSED, HISTORY_PAUSED, SERVER_STORAGE_PAUSED,
}

internal enum class MmsHealthNote { ACCESS_NEEDED, LOCKED, NOT_COPIED, SKIPPED, BOTH }

internal data class MmsHealthPresentation(
    val status: MmsPresentationStatus,
    val note: MmsHealthNote? = null,
    val showRescan: Boolean = false,
)

internal fun mmsHealthPresentation(
    enabled: Boolean,
    receiveGranted: Boolean,
    health: MmsHealth,
): MmsHealthPresentation {
    if (!enabled) return MmsHealthPresentation(MmsPresentationStatus.OFF)
    if (!receiveGranted) return MmsHealthPresentation(MmsPresentationStatus.ACCESS_NEEDED, MmsHealthNote.ACCESS_NEEDED)
    if (health.databaseUnavailable) return MmsHealthPresentation(MmsPresentationStatus.LOCKED, MmsHealthNote.LOCKED)

    val notCopied = health.blocked + health.unavailable > 0 || health.transferReasons.any { !it.endsWith(": transient") }
    val skipped = health.quarantinedCount > 0L
    val note = when {
        notCopied && skipped -> MmsHealthNote.BOTH
        notCopied -> MmsHealthNote.NOT_COPIED
        skipped -> MmsHealthNote.SKIPPED
        else -> null
    }
    val status = when (health.backfillStatus ?: MmsBackfillStatus.GettingReady) {
        MmsBackfillStatus.PausedPhoneStorage -> MmsPresentationStatus.PHONE_STORAGE_PAUSED
        MmsBackfillStatus.OlderPausedHistoryLimit -> MmsPresentationStatus.OLDER_HISTORY_PAUSED
        MmsBackfillStatus.PausedHistoryLimit -> MmsPresentationStatus.HISTORY_PAUSED
        MmsBackfillStatus.OlderPausedServerStorage -> MmsPresentationStatus.SERVER_STORAGE_PAUSED
        MmsBackfillStatus.GettingReady -> MmsPresentationStatus.GETTING_READY
        MmsBackfillStatus.CopyingOlder -> MmsPresentationStatus.COPYING_OLDER
        is MmsBackfillStatus.Copying -> MmsPresentationStatus.COPYING
        MmsBackfillStatus.UpToDate -> when {
            health.pending > 0 -> MmsPresentationStatus.COPYING
            note != null -> MmsPresentationStatus.MOSTLY_UP_TO_DATE
            else -> MmsPresentationStatus.UP_TO_DATE
        }
    }
    return MmsHealthPresentation(status, note, health.blocked > 0 || health.unavailable > 0 || skipped)
}

@Composable
private fun MmsPresentationStatus.text(): String = when (this) {
    MmsPresentationStatus.OFF -> stringResource(R.string.peppy_toggle_android_mms_status_off)
    MmsPresentationStatus.ACCESS_NEEDED -> stringResource(R.string.peppy_toggle_android_mms_status_access_needed)
    MmsPresentationStatus.LOCKED -> stringResource(R.string.peppy_toggle_android_mms_status_locked)
    MmsPresentationStatus.GETTING_READY -> stringResource(R.string.peppy_toggle_android_mms_status_getting_ready)
    MmsPresentationStatus.COPYING_OLDER -> stringResource(R.string.peppy_toggle_android_mms_status_copying_older)
    MmsPresentationStatus.COPYING -> stringResource(R.string.peppy_toggle_android_mms_status_copying)
    MmsPresentationStatus.UP_TO_DATE -> stringResource(R.string.peppy_toggle_android_mms_status_up_to_date)
    MmsPresentationStatus.MOSTLY_UP_TO_DATE -> stringResource(R.string.peppy_toggle_android_mms_status_mostly_up_to_date)
    MmsPresentationStatus.PHONE_STORAGE_PAUSED -> stringResource(R.string.peppy_toggle_android_mms_status_paused_phone_storage)
    MmsPresentationStatus.OLDER_HISTORY_PAUSED -> stringResource(R.string.peppy_toggle_android_mms_status_older_paused_history_limit)
    MmsPresentationStatus.HISTORY_PAUSED -> stringResource(R.string.peppy_toggle_android_mms_status_paused_history_limit)
    MmsPresentationStatus.SERVER_STORAGE_PAUSED -> stringResource(R.string.peppy_toggle_android_mms_status_older_paused_server_storage)
}

@Composable
private fun MmsHealthNote.text(): String = when (this) {
    MmsHealthNote.ACCESS_NEEDED -> stringResource(R.string.peppy_toggle_android_mms_note_access_needed)
    MmsHealthNote.LOCKED -> stringResource(R.string.peppy_toggle_android_mms_note_locked)
    MmsHealthNote.NOT_COPIED -> stringResource(R.string.peppy_toggle_android_mms_note_not_copied)
    MmsHealthNote.SKIPPED -> stringResource(R.string.peppy_toggle_android_mms_note_skipped)
    MmsHealthNote.BOTH -> stringResource(R.string.peppy_toggle_android_mms_note_both)
}

/** Bounded, content-free transfer failures shown only on this phone. */
internal class MmsTransferHealth(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("peppy-mms-transfer-health", Context.MODE_PRIVATE)

    fun record(failures: List<MmsTransferFailure>) {
        if (failures.isEmpty()) {
            prefs.edit().remove(ISSUES).apply()
            return
        }
        val entries = JSONArray()
        failures.take(MAX_ISSUES).forEach { failure ->
            entries.put(JSONObject().put("attachment", failure.attachmentId).put("operation", failure.operation).put("reason", failure.reason))
        }
        prefs.edit().putString(ISSUES, entries.toString()).apply()
    }

    fun reasons(): List<String> = try {
        val entries = JSONArray(prefs.getString(ISSUES, "[]"))
        (0 until entries.length()).mapNotNull { index ->
            val entry = entries.optJSONObject(index) ?: return@mapNotNull null
            val operation = entry.optString("operation")
            val reason = entry.optString("reason")
            if (operation in OPERATIONS && reason in REASONS) "$operation: $reason" else null
        }
    } catch (_: Exception) {
        emptyList()
    }

    private companion object {
        const val ISSUES = "issues"
        const val MAX_ISSUES = 8
        val OPERATIONS = setOf("upload", "download")
        val REASONS = setOf("transient", "auth", "quota", "permanent", "invalid_media")
    }
}

internal fun mmsReceiveGranted(context: Context) =
    context.checkSelfPermission(Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED &&
        context.checkSelfPermission(Manifest.permission.RECEIVE_MMS) == PackageManager.PERMISSION_GRANTED

private fun loadMmsHealth(context: Context, databaseOpen: Boolean): MmsHealth {
    if (!databaseOpen) return MmsHealth(databaseUnavailable = true, transferReasons = MmsTransferHealth(context).reasons())
    val client = NativeGateway.open(context) ?: return MmsHealth(databaseUnavailable = true, transferReasons = MmsTransferHealth(context).reasons())
    val acquisitions = client.mmsAcquisitions(1000uL)
    val backfill = client.mmsBackfillStatus(MmsPreferences(context).sourceGeneration, MmsCapture.PROVIDER_SCOPE)
    return mmsHealthFromNative(acquisitions, backfill, MmsTransferHealth(context).reasons())
}

internal fun mmsHealthFromNative(
    acquisitions: List<uniffi.peppy_mobile_bindings.NativeMmsAcquisition>,
    backfill: NativeMmsBackfillStatus,
    transferReasons: List<String>,
): MmsHealth {
    val incomplete = acquisitions.filter { it.state != NativeMmsAcquisitionState.COMPLETE }
    val status = when (backfill.pause) {
        NativeMmsBackfillPause.LOCAL_CAPACITY -> MmsBackfillStatus.PausedPhoneStorage
        NativeMmsBackfillPause.RECOVERY_CAPACITY -> MmsBackfillStatus.OlderPausedHistoryLimit
        NativeMmsBackfillPause.ABSOLUTE_RECOVERY_CAPACITY -> MmsBackfillStatus.PausedHistoryLimit
        NativeMmsBackfillPause.REMOTE_STORAGE -> MmsBackfillStatus.OlderPausedServerStorage
        null -> when {
            !backfill.baselineInitialized -> MmsBackfillStatus.GettingReady
            !backfill.historyComplete -> MmsBackfillStatus.CopyingOlder
            backfill.pendingCount > 0uL -> MmsBackfillStatus.Copying(backfill.pendingCount)
            else -> MmsBackfillStatus.UpToDate
        }
    }
    return MmsHealth(
        pending = incomplete.count { it.state == NativeMmsAcquisitionState.PENDING },
        blocked = incomplete.count { it.state == NativeMmsAcquisitionState.BLOCKED },
        unavailable = incomplete.count { it.state == NativeMmsAcquisitionState.UNAVAILABLE },
        transferReasons = transferReasons,
        backfillStatus = status,
        quarantinedCount = backfill.quarantinedCount.toLong(),
    )
}

internal fun ownAddressError(error: Throwable?): String = when (error) {
    is MobileBindingsException.InvalidRequest -> "This number could not be saved for the selected SIM."
    is MobileBindingsException.Closed -> "The encrypted database is unavailable. Import or unlock this phone first."
    is MobileBindingsException.InvalidDatabaseKey,
    is MobileBindingsException.WrongDatabaseKey,
    is MobileBindingsException.Database,
    is MobileBindingsException.UnsupportedSchema,
    is MobileBindingsException.Crypto -> "The encrypted database is unavailable. Try again after reopening the phone."
    else -> "Could not save the number securely. Try again."
}

/** Local-only companion controls. Own addresses are committed directly to SQLCipher, never prefs. */
@Composable
internal fun MmsSettings(
    context: Context,
    refresh: Int,
    databaseOpen: Boolean,
    onChanged: () -> Unit,
    detailsOnly: Boolean = false,
) {
    val preferences = remember { MmsPreferences(context) }
    var enabled by remember(refresh) { mutableStateOf(preferences.enabled) }
    var mediaWifiOnly by remember { mutableStateOf<Boolean?>(null) }
    // The core-backed policy read takes the client lock; keep it off the main thread.
    LaunchedEffect(refresh) { mediaWifiOnly = withContext(Dispatchers.IO) { GatewayPolicyHost(context).mediaWifiOnly } }
    val scope = rememberCoroutineScope()
    var health by remember { mutableStateOf(MmsHealth()) }
    val route = SimRoutes.current().singleOrNull()
    val lifecycleOwner = LocalLifecycleOwner.current
    var permissionRefresh by remember { mutableIntStateOf(0) }
    var consent by remember { mutableStateOf(ConsentGatedToggle()) }
    var permissionHint by remember { mutableStateOf<String?>(null) }
    var permissionDenied by remember { mutableStateOf(false) }
    var restrictedAccess by remember { mutableStateOf(RestrictedMessageAccess.UNKNOWN) }
    val hasAdvancedFeaturesSetup = restrictedSetupAvailable(context)
    var rescanFeedback by remember { mutableStateOf<String?>(null) }
    var rescanPending by remember { mutableStateOf(false) }
    val rescanQueued = stringResource(R.string.peppy_mms_restricted_rescan_queued)
    val rescanAlreadyQueued = stringResource(R.string.peppy_mms_restricted_rescan_already_queued)
    val rescanUnavailable = stringResource(R.string.peppy_mms_restricted_rescan_unavailable)
    val rescanFailed = stringResource(R.string.peppy_mms_restricted_rescan_failed)
    val deniedHint = stringResource(R.string.peppy_toggle_android_mms_denied_hint)
    val smsGuardFailedHint = stringResource(R.string.peppy_toggle_android_sms_guard_failed)
    val smsPolicyAction = remember(context) {
        SmsCapturePolicyAction(
            writeAndRead = { value ->
                withContext(Dispatchers.IO) {
                    GatewayPolicyHost(context).smsCaptureEnabled = value
                    GatewayPolicyHost(context).smsCaptureEnabled == value
                }
            },
            onActivated = {},
        )
    }
    fun consumePermissionResult(grantedAll: Boolean) {
        if (consent.phase != ConsentPhase.AWAITING_ACCESS) return
        permissionDenied = !grantedAll
        val transition = consent.accessResult(grantedAll)
        consent = transition.state
        if (transition.effect == ConsentEffect.APPLY) {
            preferences.enabled = true
            enabled = preferences.enabled
            MmsCaptureWork.onPermissionOrForeground(context)
            onChanged()
        } else {
            permissionHint = deniedHint
        }
        consent = consent.cancel()
    }
    val permissionRequest = androidx.activity.compose.rememberLauncherForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions(),
    ) { results -> consumePermissionResult(MMS_PERMISSIONS.all { results[it] == true || granted(context, it) }) }
    val settingsRequest = androidx.activity.compose.rememberLauncherForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.StartActivityForResult(),
    ) { consumePermissionResult(MMS_PERMISSIONS.all { granted(context, it) }) }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) permissionRefresh++ }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    val receiveGranted = permissionRefresh.let { mmsReceiveGranted(context) }
    DisposableEffect(Unit) { onDispose { consent = consent.cancel() } }

    LaunchedEffect(refresh, enabled, databaseOpen) {
        health = withContext(Dispatchers.IO) { loadMmsHealth(context, databaseOpen) }
    }
    LaunchedEffect(permissionRefresh, refresh) {
        restrictedAccess = withContext(Dispatchers.IO) { restrictedMessageAccess(context) }
    }

    if (!detailsOnly) {
        Column(
            Modifier.fillMaxWidth().testTag("mms-settings-section"),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            MmsCaptureControls(
                enabled = enabled && receiveGranted,
                onEnabledChanged = { value ->
                    if (!value) {
                        preferences.enabled = false
                        enabled = preferences.enabled
                        MmsCaptureWork.onPermissionOrForeground(context)
                        onChanged()
                    } else consent = consent.requestEnable()
                },
                hint = permissionHint,
            )
            SettingToggleRow(
                title = stringResource(R.string.peppy_media_wifi_only),
                checked = mediaWifiOnly == true,
                enabled = mediaWifiOnly != null,
                onCheckedChange = { value ->
                    mediaWifiOnly = null
                    scope.launch {
                        mediaWifiOnly = withContext(Dispatchers.IO) {
                            GatewayPolicyHost(context).mediaWifiOnly = value
                            GatewayPolicyHost(context).mediaWifiOnly
                        }
                    }
                },
                tag = "mms-wifi-only-row",
                titleTrailing = {
                    InfoButton(
                        topic = stringResource(R.string.peppy_media_wifi_only),
                        title = stringResource(R.string.peppy_media_wifi_only),
                        body = stringResource(R.string.peppy_help_android_mms_wifi),
                        tag = "info-mms-wifi",
                    )
                },
            )
        }
    } else {
        Section(
            "mms-details-section",
            stringResource(R.string.peppy_toggle_android_mms_title),
            helpBody = stringResource(R.string.peppy_help_android_mms_capture),
        ) {
            StatusRow(
                "mms-sim-status",
                stringResource(R.string.peppy_toggle_android_mms_sim_label),
                route?.label ?: stringResource(R.string.peppy_toggle_android_sim_missing),
            )
            OwnNumberSettings(
                context,
                databaseOpen,
                route,
                trailingAction = {
                    TextButton(
                        onClick = { MmsCaptureWork.refresh(context); onChanged() },
                        enabled = enabled && receiveGranted,
                        modifier = Modifier.heightIn(min = 48.dp).testTag("mms-refresh-pending"),
                    ) { Text(stringResource(R.string.peppy_toggle_android_mms_refresh)) }
                },
            )
            val presentation = mmsHealthPresentation(enabled, receiveGranted, health)
            Column(Modifier.fillMaxWidth().testTag("mms-health-list"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                StatusRow(
                    "mms-backfill-status",
                    stringResource(R.string.peppy_toggle_android_mms_status_label),
                    presentation.status.text(),
                )
                presentation.note?.let {
                    Text(it.text(), Modifier.testTag("mms-health-note"), style = MaterialTheme.typography.bodySmall)
                }
            }
            RestrictedMessageAccessControls(
                context = context,
                access = restrictedAccess,
                showAccessStatus = !hasAdvancedFeaturesSetup,
                showRescan = presentation.showRescan && enabled && receiveGranted && !health.databaseUnavailable,
                enabled = canRequestProviderRescan(enabled, receiveGranted, databaseOpen, restrictedAccess),
                pending = rescanPending,
                feedback = rescanFeedback,
                startRescan = {
                    rescanPending = true
                    scope.launch {
                        val outcome = withContext(Dispatchers.IO) {
                            val currentAccess = restrictedMessageAccess(context)
                            if (!canRequestProviderRescan(preferences.enabled, mmsReceiveGranted(context), databaseOpen, currentAccess) ||
                                GatewayPolicyHost(context).decision()?.captureMms != true
                            ) {
                                ProviderRescanRequestOutcome.UNAVAILABLE
                            } else {
                                val client = NativeGateway.producerClient(context)
                                    ?: return@withContext ProviderRescanRequestOutcome.UNAVAILABLE
                                requestProviderRescan(
                                    currentAccess,
                                    observedMaxId = { MmsCapture.currentHighWater(context) },
                                    request = { observedMaxId ->
                                        if (!canRequestProviderRescan(
                                                preferences.enabled,
                                                mmsReceiveGranted(context),
                                                databaseOpen,
                                                restrictedMessageAccess(context),
                                            ) || GatewayPolicyHost(context).decision()?.captureMms != true ||
                                            NativeGateway.producerClient(context) !== client
                                        ) {
                                            throw SecurityException("MMS capture is no longer available")
                                        }
                                        client.requestMmsProviderRescan(
                                            preferences.sourceGeneration,
                                            MmsCapture.PROVIDER_SCOPE,
                                            observedMaxId,
                                        )
                                    },
                                    enqueue = { MmsCaptureWork.enqueue(context.applicationContext) },
                                )
                            }
                        }
                        restrictedAccess = withContext(Dispatchers.IO) { restrictedMessageAccess(context) }
                        rescanFeedback = when (outcome) {
                            ProviderRescanRequestOutcome.QUEUED -> rescanQueued
                            ProviderRescanRequestOutcome.ALREADY_QUEUED -> rescanAlreadyQueued
                            ProviderRescanRequestOutcome.UNAVAILABLE -> rescanUnavailable
                            ProviderRescanRequestOutcome.FAILED -> rescanFailed
                        }
                        rescanPending = false
                    }
                },
            )
            }
        }
    if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_mms_consent_title),
        body = stringResource(R.string.peppy_mms_capture_disclosure),
        tag = "mms-consent-sheet",
        onDismiss = { consent = consent.cancel() },
        onConfirm = {
            val continueRequest = {
                val transition = consent.agree(receiveGranted)
                consent = transition.state
                if (transition.effect == ConsentEffect.REQUEST_ACCESS) {
                    val activity = context.contextActivity()
                    val needsSettings = permissionDenied && MMS_PERMISSIONS.any {
                        !granted(context, it) && activity?.shouldShowRequestPermissionRationale(it) == false
                    }
                    if (needsSettings) {
                        settingsRequest.launch(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null)))
                    } else permissionRequest.launch(MMS_PERMISSIONS)
                }
                if (transition.effect == ConsentEffect.APPLY) {
                    preferences.enabled = true
                    enabled = preferences.enabled
                    MmsCaptureWork.onPermissionOrForeground(context)
                    onChanged()
                }
            }
            // READ_SMS can activate the existing SMS policy. Fail closed before asking for MMS.
            if (!mmsReceiveGranted(context) && !smsReadGranted(context)) {
                scope.launch {
                    try {
                        val inactive = smsPolicyAction.prepareFreshReadGrant(readGranted = false, requestAccess = continueRequest)
                        if (!inactive) {
                            consent = consent.cancel()
                            permissionHint = smsGuardFailedHint
                        }
                    } catch (error: CancellationException) {
                        throw error
                    } catch (_: Exception) {
                        consent = consent.cancel()
                        permissionHint = smsGuardFailedHint
                    }
                }
            } else continueRequest()
        },
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = if (receiveGranted) null else stringResource(R.string.peppy_toggle_android_mms_next) +
            if (!smsReadGranted(context)) "\n\n" + stringResource(R.string.peppy_toggle_android_mms_sms_stays_off) else "",
        disclosureTag = "mms-capture-disclosure",
    )
}

@Composable
private fun RestrictedMessageAccessControls(
    context: Context,
    access: RestrictedMessageAccess,
    showAccessStatus: Boolean,
    showRescan: Boolean,
    enabled: Boolean,
    pending: Boolean,
    feedback: String?,
    startRescan: () -> Unit,
) {
    val command = remember(context.packageName) { restrictedMessageAppOpCommand(context.packageName) }
    var commandCopied by remember { mutableStateOf(false) }
    if (showAccessStatus) {
        RestrictedMessageAccessStatus(access)
    }
    if (access == RestrictedMessageAccess.ALLOWED && showRescan) {
        OutlinedButton(
            onClick = startRescan,
            enabled = enabled && !pending,
            modifier = Modifier.testTag("mms-restricted-message-rescan"),
        ) { Text(stringResource(R.string.peppy_mms_restricted_rescan)) }
    } else if (showAccessStatus && access == RestrictedMessageAccess.NOT_ALLOWED) {
        Text(command, Modifier.testTag("mms-restricted-message-command"), style = MaterialTheme.typography.bodySmall)
        OutlinedButton(
            onClick = {
                val clipboard = context.getSystemService(ClipboardManager::class.java)
                commandCopied = try {
                    clipboard?.setPrimaryClip(ClipData.newPlainText("Restricted message access command", command))
                    clipboard != null
                } catch (_: Exception) {
                    false
                }
            },
            modifier = Modifier.testTag("mms-restricted-message-copy-command"),
        ) { Text(stringResource(R.string.peppy_mms_restricted_copy_command)) }
    }
    if (showAccessStatus && commandCopied) Text(stringResource(R.string.peppy_mms_restricted_command_copied), Modifier.testTag("mms-restricted-message-command-copied"), style = MaterialTheme.typography.bodySmall)
    feedback?.let {
        Text(
            it,
            Modifier.testTag("mms-restricted-message-feedback").semantics { liveRegion = LiveRegionMode.Polite },
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

@Composable
internal fun RestrictedMessageAccessStatus(access: RestrictedMessageAccess) {
    val title = stringResource(R.string.peppy_mms_restricted_title)
    val status = when (access) {
        RestrictedMessageAccess.ALLOWED -> stringResource(R.string.peppy_mms_restricted_status_allowed)
        RestrictedMessageAccess.NOT_ALLOWED -> stringResource(R.string.peppy_mms_restricted_status_not_allowed)
        RestrictedMessageAccess.UNSUPPORTED -> stringResource(R.string.peppy_mms_restricted_status_unsupported)
        RestrictedMessageAccess.UNKNOWN -> stringResource(R.string.peppy_mms_restricted_status_unknown)
    }
    val help = buildString {
        append(stringResource(R.string.peppy_mms_restricted_help))
        if (access == RestrictedMessageAccess.UNSUPPORTED) {
            append("\n\n")
            append(stringResource(R.string.peppy_mms_restricted_unsupported_help))
        }
    }
    val density = LocalDensity.current
    BoxWithConstraints(Modifier.testTag("mms-restricted-message-access")) {
        val compact = maxWidth < 360.dp || density.fontScale > 1.15f
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (compact) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    RestrictedMessageAccessLabel(title, help)
                }
                Text(status, style = MaterialTheme.typography.bodySmall)
            } else {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    RestrictedMessageAccessLabel(title, help, Modifier.weight(1f))
                    Text(status, Modifier.widthIn(max = 160.dp), style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun RestrictedMessageAccessLabel(title: String, help: String, modifier: Modifier = Modifier) {
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        Text(title, Modifier.weight(1f, fill = false))
        InfoButton(
            topic = title,
            title = title,
            body = help,
            tag = "info-mms-restricted-message-access",
        )
    }
}

@Composable
internal fun MmsCaptureControls(
    enabled: Boolean,
    onEnabledChanged: (Boolean) -> Unit,
    hint: String?,
) {
    SettingToggleRow(
        stringResource(R.string.peppy_toggle_android_mms_title), enabled, true, onEnabledChanged, "mms-enable-row",
        hint ?: stringResource(R.string.peppy_toggle_android_mms_provider_hint),
    )
}
