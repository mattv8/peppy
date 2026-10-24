package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import org.json.JSONArray
import org.json.JSONObject
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.Switch
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.ui.InfoButton
import androidx.compose.material3.MaterialTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPause
import uniffi.peppy_mobile_bindings.NativeMmsBackfillStatus

internal data class MmsHealth(
    val pending: Int = 0,
    val blocked: Int = 0,
    val unavailable: Int = 0,
    val reasons: List<String> = emptyList(),
    val transferReasons: List<String> = emptyList(),
    val databaseUnavailable: Boolean = false,
    val backfillStatus: String? = null,
    val quarantinedCount: Long = 0L,
)

/** Bounded, content-free transfer failures shown only on this phone. */
internal class MmsTransferHealth(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("peppy-mms-transfer-health", Context.MODE_PRIVATE)

    fun record(failures: List<MmsTransferFailure>) {
        if (failures.isEmpty()) return
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
        NativeMmsBackfillPause.LOCAL_CAPACITY -> "Paused: phone storage capacity"
        NativeMmsBackfillPause.RECOVERY_CAPACITY -> "Paused: recovery capacity"
        NativeMmsBackfillPause.REMOTE_STORAGE -> "History paused: vault storage capacity"
        null -> when {
            !backfill.baselineInitialized -> "Waiting for the first provider scan"
            !backfill.historyComplete -> "Scanning MMS history"
            backfill.pendingCount > 0uL -> "Processing ${backfill.pendingCount} pending MMS"
            else -> "MMS capture is up to date"
        }
    }
    return MmsHealth(
        pending = incomplete.count { it.state == NativeMmsAcquisitionState.PENDING },
        blocked = incomplete.count { it.state == NativeMmsAcquisitionState.BLOCKED },
        unavailable = incomplete.count { it.state == NativeMmsAcquisitionState.UNAVAILABLE },
        reasons = incomplete.mapNotNull { it.reason?.take(120) }.distinct().take(8),
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
    requestPermissions: () -> Unit,
    onChanged: () -> Unit,
) {
    val preferences = remember { MmsPreferences(context) }
    var enabled by remember(refresh) { mutableStateOf(preferences.enabled) }
    var mediaWifiOnly by remember(refresh) { mutableStateOf(GatewayPolicyHost(context).mediaWifiOnly) }
    var health by remember { mutableStateOf(MmsHealth()) }
    val route = SimRoutes.current().singleOrNull()
    val lifecycleOwner = LocalLifecycleOwner.current
    var permissionRefresh by remember { mutableIntStateOf(0) }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) permissionRefresh++ }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    val receiveGranted = permissionRefresh.let { mmsReceiveGranted(context) }

    LaunchedEffect(refresh, enabled, databaseOpen) {
        health = withContext(Dispatchers.IO) { loadMmsHealth(context, databaseOpen) }
    }

    Section("mms-settings-section", "MMS") {
        MmsCaptureControls(
            enabled = enabled,
            receiveGranted = receiveGranted,
            onEnabledChanged = { value ->
                preferences.enabled = value
                enabled = preferences.enabled
                MmsCaptureWork.onPermissionOrForeground(context)
                onChanged()
            },
            onRequestPermissions = requestPermissions,
        )
        StatusRow("mms-sim-status", "Current SIM", route?.label ?: "No default SMS SIM")
        OwnNumberSettings(context, databaseOpen, route)
        Row(Modifier.fillMaxWidth().testTag("mms-wifi-only-row"), horizontalArrangement = Arrangement.SpaceBetween) {
            Column(Modifier.weight(1f)) {
                Text(stringResource(R.string.peppy_media_wifi_only))
            }
            InfoButton(
                topic = stringResource(R.string.peppy_media_wifi_only),
                title = stringResource(R.string.peppy_media_wifi_only),
                body = stringResource(R.string.peppy_help_android_mms_wifi),
                tag = "info-mms-wifi",
            )
            Switch(mediaWifiOnly, { value -> GatewayPolicyHost(context).mediaWifiOnly = value; mediaWifiOnly = value }, modifier = Modifier.testTag("mms-wifi-only-switch"))
        }
        OutlinedButton(
            onClick = { MmsCaptureWork.refresh(context); onChanged() },
            enabled = enabled && receiveGranted,
            modifier = Modifier.testTag("mms-refresh-pending"),
        ) { Text("Refresh pending MMS") }
        Column(Modifier.fillMaxWidth().testTag("mms-health-list"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            when {
                health.databaseUnavailable -> Text("Encrypted database unavailable. Acquisition health cannot be read.")
                health.pending == 0 && health.blocked == 0 && health.unavailable == 0 -> Text("No pending acquisitions.")
                else -> Text("Phone-only acquisition health: ${health.pending} pending, ${health.blocked} blocked, ${health.unavailable} unavailable.")
            }
            health.backfillStatus?.let { Text(it, Modifier.testTag("mms-backfill-status"), style = MaterialTheme.typography.bodySmall) }
            if (health.quarantinedCount > 0L) {
                Text(
                    "Quarantined records: ${health.quarantinedCount}",
                    Modifier.testTag("mms-quarantine-status"),
                    style = MaterialTheme.typography.bodySmall,
                )
                Text(
                    "These phone records were skipped because they could not be matched safely. Existing messages were kept.",
                    Modifier.testTag("mms-quarantine-explanation"),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            health.reasons.forEach { Text(it, Modifier.testTag("mms-health-reason"), style = MaterialTheme.typography.bodySmall) }
            health.transferReasons.forEach { Text("Transfer $it", Modifier.testTag("mms-transfer-health-reason"), style = MaterialTheme.typography.bodySmall) }
        }
    }
}

@Composable
internal fun MmsCaptureControls(
    enabled: Boolean,
    receiveGranted: Boolean,
    onEnabledChanged: (Boolean) -> Unit,
    onRequestPermissions: () -> Unit,
) {
    Text(
        stringResource(R.string.peppy_mms_capture_disclosure),
        Modifier.testTag("mms-capture-disclosure"),
        style = MaterialTheme.typography.bodyMedium,
    )
    Row(Modifier.fillMaxWidth().testTag("mms-enable-row"), horizontalArrangement = Arrangement.SpaceBetween) {
        Text("Enable MMS capture", Modifier.weight(1f))
        InfoButton("Enable MMS capture", "Enable MMS capture", stringResource(R.string.peppy_help_android_mms_capture), "info-mms-capture")
        Switch(enabled, onEnabledChanged, modifier = Modifier.testTag("mms-enable-switch"))
    }
    StatusRow("mms-permission-status", "MMS read permission", if (receiveGranted) "Allowed" else "Not allowed")
    if (!receiveGranted) {
        OutlinedButton(onClick = onRequestPermissions, modifier = Modifier.testTag("mms-permission-button")) { Text("Allow MMS permissions") }
    }
}
