package dev.peppy.mobile.sms

import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.R
import dev.peppy.mobile.Section
import dev.peppy.mobile.ui.DetailsStatusRow as StatusRow

internal enum class SmsHistoryState {
    WAITING_FOR_PERMISSION, SYNC_OFF, WAITING_FOR_VAULT, WAITING, READING, PAUSED, CAPTURED, UNAVAILABLE, PENDING,
}

internal data class SmsHistoryCoreStatus(
    val baselineInitialized: Boolean,
    val scanning: Boolean,
    val historyComplete: Boolean,
    val paused: Boolean,
    val providerError: Boolean,
    val hasPending: Boolean,
)

internal data class SmsHistoryPresentation(
    val state: SmsHistoryState,
    val showPermission: Boolean,
    val showRepair: Boolean,
)

internal fun smsHistoryPresentation(
    smsSyncOn: Boolean,
    historyState: SmsHistoryState,
    readGranted: Boolean,
): SmsHistoryPresentation = SmsHistoryPresentation(
    state = if (smsSyncOn) historyState else SmsHistoryState.SYNC_OFF,
    showPermission = smsSyncOn,
    showRepair = smsSyncOn && !readGranted,
)

internal fun smsHistoryState(status: SmsHistoryCoreStatus): SmsHistoryState = when {
    status.providerError -> SmsHistoryState.UNAVAILABLE
    status.paused -> SmsHistoryState.PAUSED
    status.hasPending -> SmsHistoryState.PENDING
    status.historyComplete -> SmsHistoryState.CAPTURED
    status.scanning || status.baselineInitialized -> SmsHistoryState.READING
    else -> SmsHistoryState.WAITING
}

internal fun smsHistoryLabel(state: SmsHistoryState): Int = when (state) {
    SmsHistoryState.WAITING_FOR_PERMISSION -> R.string.peppy_sms_history_waiting_permission
    SmsHistoryState.SYNC_OFF -> R.string.peppy_sms_history_sync_off
    SmsHistoryState.WAITING_FOR_VAULT -> R.string.peppy_sms_history_waiting_vault
    SmsHistoryState.WAITING -> R.string.peppy_sms_history_waiting
    SmsHistoryState.READING -> R.string.peppy_sms_history_reading
    SmsHistoryState.PAUSED -> R.string.peppy_sms_history_paused
    SmsHistoryState.CAPTURED -> R.string.peppy_sms_history_captured
    SmsHistoryState.UNAVAILABLE -> R.string.peppy_sms_history_unavailable
    SmsHistoryState.PENDING -> R.string.peppy_sms_history_pending
}

@Composable
internal fun SmsSettings(
    context: Context,
    historyState: SmsHistoryState,
    smsSyncOn: Boolean = true,
    onRepairAccess: (() -> Unit)? = null,
) {
    val readGranted = smsReadGranted(context)
    val presentation = smsHistoryPresentation(smsSyncOn, historyState, readGranted)
    Section(
        "sms-history-section",
        stringResource(R.string.peppy_sms_history_title),
        helpBody = stringResource(R.string.peppy_sms_history_description) + "\n\n" +
            stringResource(R.string.peppy_sms_history_captured_footer),
        headerAction = if (presentation.showRepair && onRepairAccess != null) {
            { TextButton(onClick = onRepairAccess, modifier = Modifier.testTag("sms-history-allow")) { Text(stringResource(R.string.peppy_toggle_android_allow)) } }
        } else null,
    ) {
        if (presentation.showPermission) StatusRow(
            "sms-read-permission-status",
            stringResource(R.string.peppy_sms_history_permission_label),
            stringResource(if (readGranted) R.string.peppy_sms_history_allowed else R.string.peppy_sms_history_not_allowed),
        )
        StatusRow("sms-history-status", stringResource(R.string.peppy_sms_history_status_label), stringResource(smsHistoryLabel(presentation.state)))
        if (presentation.state == SmsHistoryState.PAUSED) Text(
            stringResource(R.string.peppy_sms_history_capacity_footer),
            Modifier.testTag("sms-history-capacity"),
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

internal fun smsHistoryState(readGranted: Boolean, capturePermitted: Boolean, databaseOpen: Boolean): SmsHistoryState = when {
    !capturePermitted -> SmsHistoryState.SYNC_OFF
    !readGranted -> SmsHistoryState.WAITING_FOR_PERMISSION
    !databaseOpen -> SmsHistoryState.WAITING_FOR_VAULT
    else -> SmsHistoryState.WAITING
}
