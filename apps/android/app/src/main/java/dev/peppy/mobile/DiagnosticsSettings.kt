package dev.peppy.mobile

import android.content.ClipData
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.ui.Alignment
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.size
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal data class DiagnosticsActions(
    val recording: Boolean,
    val canShare: Boolean,
    val canClear: Boolean,
)

internal fun diagnosticsActions(state: DiagnosticsState) = DiagnosticsActions(
    recording = state.enabled,
    canShare = state.bytes > 0 && !state.busy,
    canClear = state.bytes > 0 && !state.busy,
)

internal fun diagnosticsSize(bytes: Long): String = when {
    bytes < 1024 -> "$bytes B"
    bytes < 1024 * 1024 -> "${bytes / 1024} KiB"
    else -> String.format(java.util.Locale.US, "%.1f MiB", bytes / (1024.0 * 1024.0))
}

internal fun diagnosticsShareIntent(uri: Uri): Intent = Intent(Intent.ACTION_SEND).apply {
    type = "text/plain"
    putExtra(Intent.EXTRA_STREAM, uri)
    clipData = ClipData.newRawUri("diagnostic-log", uri)
    addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
}

internal fun launchDiagnosticsShareIfCurrent(
    uri: Uri,
    isCurrent: (Uri) -> Boolean,
    launch: (Intent) -> Unit,
) {
    if (isCurrent(uri)) launch(Intent.createChooser(diagnosticsShareIntent(uri), null))
}

@Composable
internal fun DiagnosticsSettings() {
    val state by DiagnosticsController.state.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    DiagnosticsSettingsContent(
        state = state,
        onEnabledChanged = DiagnosticsController::setEnabled,
        onShare = {
            scope.launch {
                val uri = withContext(Dispatchers.IO) { DiagnosticsController.export() } ?: return@launch
                launchDiagnosticsShareIfCurrent(uri, DiagnosticsController::isExportCurrent, context::startActivity)
            }
        },
        onClear = {
            scope.launch {
                withContext(Dispatchers.IO) { DiagnosticsController.clear() }
            }
        },
    )
}

@Composable
internal fun DiagnosticsSettingsContent(
    state: DiagnosticsState,
    onEnabledChanged: (Boolean) -> Unit,
    onShare: () -> Unit,
    onClear: () -> Unit,
) {
    val actions = diagnosticsActions(state)
    var clearConfirmationVisible by remember { mutableStateOf(false) }
    val verboseLoggingLabel = stringResource(R.string.peppy_diagnostics_verbose_sync_logging)
    val workingLabel = stringResource(R.string.peppy_diagnostics_working)

    Section(
        "diagnostics-section",
        stringResource(R.string.peppy_diagnostics_title),
        helpBody = stringResource(R.string.peppy_diagnostics_overhead),
    ) {
        Row(
            Modifier.fillMaxWidth().testTag("diagnostics-verbose-sync-logging"),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(Modifier.weight(1f)) {
                Text(verboseLoggingLabel)
                Text(
                    stringResource(R.string.peppy_diagnostics_verbose_sync_logging_hint),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = state.enabled,
                onCheckedChange = onEnabledChanged,
                enabled = !state.busy,
                modifier = Modifier
                    .testTag("diagnostics-verbose-sync-logging-switch")
                    .semantics { contentDescription = verboseLoggingLabel },
            )
        }
        StatusRow(
            "diagnostics-recording-status",
            stringResource(R.string.peppy_diagnostics_recording),
            stringResource(if (actions.recording) R.string.peppy_diagnostics_active else R.string.peppy_diagnostics_off_retained),
        )
        StatusRow(
            "diagnostics-retained-size",
            stringResource(R.string.peppy_diagnostics_retained_size),
            "${diagnosticsSize(state.bytes)} / ${stringResource(R.string.peppy_diagnostics_retained_size_limit)}",
        )
        StatusRow(
            "diagnostics-dropped-events",
            stringResource(R.string.peppy_diagnostics_dropped_events),
            state.dropped.toString(),
        )
        Text(
            stringResource(R.string.peppy_diagnostics_privacy),
            Modifier.testTag("diagnostics-privacy-text"),
            style = MaterialTheme.typography.bodySmall,
        )
        state.error?.let { error ->
            Text(
                stringResource(
                    when (error) {
                        DiagnosticsError.STORAGE -> R.string.peppy_diagnostics_error_storage
                        DiagnosticsError.EXPORT -> R.string.peppy_diagnostics_error_export
                        DiagnosticsError.CLEAR -> R.string.peppy_diagnostics_error_clear
                    },
                ),
                Modifier
                    .testTag("diagnostics-error")
                    .semantics { liveRegion = LiveRegionMode.Assertive },
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(
                onClick = onShare,
                enabled = actions.canShare,
                modifier = Modifier.testTag("diagnostics-share-log"),
            ) { Text(stringResource(R.string.peppy_diagnostics_share_log)) }
            OutlinedButton(
                onClick = { clearConfirmationVisible = true },
                enabled = actions.canClear,
                modifier = Modifier.testTag("diagnostics-clear-logs"),
            ) { Text(stringResource(R.string.peppy_diagnostics_clear_logs)) }
            if (state.busy) {
                CircularProgressIndicator(
                    Modifier
                        .size(24.dp)
                        .testTag("diagnostics-busy")
                        .semantics {
                            contentDescription = workingLabel
                            liveRegion = LiveRegionMode.Polite
                        },
                    strokeWidth = 2.dp,
                )
            }
        }
    }

    if (clearConfirmationVisible) {
        AlertDialog(
            onDismissRequest = { clearConfirmationVisible = false },
            title = { Text(stringResource(R.string.peppy_diagnostics_clear_confirm_title)) },
            text = { Text(stringResource(R.string.peppy_diagnostics_clear_confirm_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        clearConfirmationVisible = false
                        onClear()
                    },
                    modifier = Modifier.testTag("diagnostics-clear-confirm"),
                ) { Text(stringResource(R.string.peppy_diagnostics_clear_confirm)) }
            },
            dismissButton = {
                TextButton(
                    onClick = { clearConfirmationVisible = false },
                    modifier = Modifier.testTag("diagnostics-clear-cancel"),
                ) { Text(stringResource(R.string.peppy_cancel)) }
            },
            modifier = Modifier.testTag("diagnostics-clear-confirmation"),
        )
    }
}
