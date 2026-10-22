package dev.peppy.mobile

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp

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
    needsPermissionSettings: Boolean,
    requestPermission: () -> Unit,
    openPermissionSettings: () -> Unit,
) {
    val readGranted = smsReadGranted(context)
    Section("sms-history-section", stringResource(R.string.peppy_sms_history_title)) {
        Text(stringResource(R.string.peppy_sms_history_description), style = MaterialTheme.typography.bodyMedium)
        StatusRow(
            "sms-read-permission-status",
            stringResource(R.string.peppy_sms_history_permission_label),
            stringResource(if (readGranted) R.string.peppy_sms_history_allowed else R.string.peppy_sms_history_not_allowed),
        )
        if (!readGranted) {
            OutlinedButton(
                onClick = if (needsPermissionSettings) openPermissionSettings else requestPermission,
                modifier = Modifier.testTag("sms-read-permission-button"),
            ) {
                Text(stringResource(if (needsPermissionSettings) R.string.peppy_sms_history_open_settings else R.string.peppy_sms_history_allow_access))
            }
        }
        StatusRow("sms-history-status", stringResource(R.string.peppy_sms_history_status_label), stringResource(smsHistoryLabel(historyState)))
        Column(Modifier.fillMaxWidth().testTag("sms-history-explanation"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(stringResource(R.string.peppy_sms_history_captured_footer), style = MaterialTheme.typography.bodySmall)
            if (historyState == SmsHistoryState.PAUSED) Text(stringResource(R.string.peppy_sms_history_capacity_footer), style = MaterialTheme.typography.bodySmall)
        }
    }
}

internal fun smsHistoryState(readGranted: Boolean, capturePermitted: Boolean, databaseOpen: Boolean): SmsHistoryState = when {
    !readGranted -> SmsHistoryState.WAITING_FOR_PERMISSION
    !capturePermitted -> SmsHistoryState.SYNC_OFF
    !databaseOpen -> SmsHistoryState.WAITING_FOR_VAULT
    else -> SmsHistoryState.WAITING
}
