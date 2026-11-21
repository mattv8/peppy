package dev.peppy.mobile.diagnostics

import android.content.ClipData
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
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
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.approval.actions
import dev.peppy.mobile.ui.DetailsStatusRow
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.SettingsGroup
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal data class DiagnosticsActions(
    val canShare: Boolean,
    val canClear: Boolean,
)

internal fun diagnosticsActions(state: DiagnosticsState) = DiagnosticsActions(
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

@OptIn(ExperimentalLayoutApi::class)
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

    SettingsGroup(
        "diagnostics-section",
        stringResource(R.string.peppy_diagnostics_title),
    ) {
        SettingToggleRow(
            title = verboseLoggingLabel,
            tag = "diagnostics-verbose-sync-logging",
            checked = state.enabled,
            enabled = !state.busy,
            onCheckedChange = onEnabledChanged,
            subtitle = stringResource(if (state.enabled) R.string.peppy_diagnostics_recording else R.string.peppy_settings_off),
            titleTrailing = {
                InfoButton(
                    topic = verboseLoggingLabel,
                    title = verboseLoggingLabel,
                    body = stringResource(R.string.peppy_diagnostics_verbose_sync_logging_hint) + "\n\n" + stringResource(R.string.peppy_diagnostics_overhead),
                    tag = "info-diagnostics-section",
                )
            },
        )
        DetailsStatusRow(
            "diagnostics-retained-size",
            stringResource(R.string.peppy_diagnostics_retained_size),
            stringResource(R.string.peppy_diagnostics_retained_of_limit, diagnosticsSize(state.bytes)),
        )
        DetailsStatusRow(
            "diagnostics-dropped-events",
            stringResource(R.string.peppy_diagnostics_dropped_events),
            state.dropped.toString(),
        )
        Text(
            stringResource(R.string.peppy_diagnostics_privacy),
            Modifier.padding(vertical = 8.dp).testTag("diagnostics-privacy-text"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
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
                    .padding(vertical = 8.dp)
                    .testTag("diagnostics-error")
                    .semantics { liveRegion = LiveRegionMode.Assertive },
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        FlowRow(
            Modifier.padding(top = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
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
                Box(Modifier.heightIn(min = 40.dp), contentAlignment = Alignment.Center) {
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
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
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
