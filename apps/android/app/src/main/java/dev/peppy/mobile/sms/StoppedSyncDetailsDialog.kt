package dev.peppy.mobile.sms

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.syncMessage
import dev.peppy.mobile.gateway.GatewayStatus

internal enum class SyncDetailsRoute { SMS, STOPPED_DETAILS }

internal fun syncDetailsRoute(status: GatewayStatus): SyncDetailsRoute =
    if (status.sharedKeysReady) SyncDetailsRoute.SMS else SyncDetailsRoute.STOPPED_DETAILS

@Composable
internal fun StoppedSyncDetailsDialog(status: GatewayStatus, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.peppy_sync_stopped_title)) },
        text = {
            Column(
                Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).testTag("sms-stopped-details-body"),
            ) {
                Text(syncMessage(status), style = MaterialTheme.typography.bodyMedium)
            }
        },
        confirmButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.heightIn(min = 48.dp).testTag("sms-stopped-details-dismiss"),
            ) {
                Text(stringResource(R.string.peppy_help_dismiss))
            }
        },
        modifier = Modifier.testTag("sms-stopped-details-dialog"),
    )
}
