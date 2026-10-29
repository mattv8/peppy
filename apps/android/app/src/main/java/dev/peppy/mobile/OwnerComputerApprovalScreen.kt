package dev.peppy.mobile

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.BackHandler
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

@Composable
internal fun OwnerComputerApprovalScreen(host: OwnerComputerApprovalHost, onDone: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var view by remember { mutableStateOf<OwnerComputerView?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var scanning by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var codesMatch by remember { mutableStateOf(false) }
    var approved by remember { mutableStateOf(false) }
    val prompt = remember { ApprovalPromptBridge() }
    val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        scanning = granted
        if (!granted) error = "camera-denied"
    }
    fun exit() {
        onDone()
    }
    fun scan(code: String) = scope.launch {
        if (busy) return@launch
        busy = true
        scanning = false
        when (val result = host.scan(code)) {
            is OwnerComputerApprovalHost.Result.View -> { view = result.view; approved = false; error = null }
            is OwnerComputerApprovalHost.Result.Approved -> { view = result.view; approved = true; error = null }
            is OwnerComputerApprovalHost.Result.Failed -> error = result.code
            OwnerComputerApprovalHost.Result.Cancelled -> Unit
        }
        busy = false
    }
    fun approve() = scope.launch {
        busy = true
        when (val result = host.approve(codesMatch, prompt)) {
            is OwnerComputerApprovalHost.Result.View -> { view = result.view; approved = false; error = null }
            is OwnerComputerApprovalHost.Result.Approved -> { view = result.view; approved = true; error = null }
            is OwnerComputerApprovalHost.Result.Failed -> error = result.code
            OwnerComputerApprovalHost.Result.Cancelled -> Unit
        }
        busy = false
    }
    prompt.request.value?.let { ApprovalPassphraseDialog(it) }
    BackHandler {
        if (scanning) scanning = false else exit()
    }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp).testTag("owner-computer-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        when {
            view == null && !scanning -> {
                Text(stringResource(R.string.peppy_owner_computer_nav_title))
                Text(stringResource(R.string.peppy_owner_computer_scan_intro))
                Button(onClick = {
                    if (context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) scanning = true
                    else cameraPermission.launch(Manifest.permission.CAMERA)
                }, enabled = !busy, modifier = Modifier.testTag("owner-computer-scan")) { Text(stringResource(R.string.peppy_owner_computer_scan_cta)) }
            }
            scanning -> {
                Text(stringResource(R.string.peppy_owner_computer_scanner_title))
                QRScanner(Modifier.fillMaxWidth().weight(1f).testTag("owner-computer-scanner"), ::scan)
                OutlinedButton(onClick = { scanning = false }, modifier = Modifier.testTag("owner-computer-scan-cancel")) { Text(stringResource(R.string.peppy_back)) }
            }
            view != null && approved -> {
                Text(stringResource(R.string.peppy_owner_computer_approved_title), Modifier.testTag("owner-computer-approved").semantics { liveRegion = LiveRegionMode.Polite })
                Text(stringResource(R.string.peppy_owner_computer_approved_body))
                Button(onClick = onDone, modifier = Modifier.testTag("owner-computer-done")) { Text(stringResource(R.string.peppy_owner_computer_done_cta)) }
            }
            else -> {
                Text(stringResource(R.string.peppy_owner_computer_verify_body))
                Text(view?.sas.orEmpty(), Modifier.testTag("owner-computer-sas"), style = androidx.compose.material3.MaterialTheme.typography.headlineMedium)
                androidx.compose.foundation.layout.Row(
                    Modifier.fillMaxWidth().testTag("owner-computer-codes-match").toggleable(codesMatch, role = Role.Checkbox) { codesMatch = it },
                    verticalAlignment = Alignment.CenterVertically,
                ) { Checkbox(codesMatch, null); Text(stringResource(R.string.peppy_owner_computer_codes_match), Modifier.padding(start = 8.dp)) }
                Button(onClick = ::approve, enabled = codesMatch && !busy, modifier = Modifier.testTag("owner-computer-approve")) { Text(stringResource(R.string.peppy_owner_computer_approve_cta)) }
            }
        }
        error?.let { code ->
            Text(ownerComputerError(code), Modifier.testTag("owner-computer-error").semantics { liveRegion = LiveRegionMode.Assertive })
            if (code == "camera-denied") {
                OutlinedButton(
                    onClick = { context.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))) },
                    modifier = Modifier.testTag("owner-computer-open-settings"),
                ) { Text(stringResource(R.string.peppy_owner_computer_open_settings_cta)) }
            }
        }
        if (!scanning && !approved) OutlinedButton(onClick = ::exit, modifier = Modifier.testTag("owner-computer-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
    }
}

@Composable
private fun ownerComputerError(code: String) = when (code) {
    "camera-denied" -> stringResource(R.string.peppy_owner_computer_camera_denied)
    "invalid-qr" -> stringResource(R.string.peppy_owner_computer_error_invalid_qr)
    "codes-do-not-match" -> stringResource(R.string.peppy_owner_computer_error_codes_mismatch)
    "secure-store-unavailable" -> stringResource(R.string.peppy_owner_computer_error_secure_store)
    "lock-required" -> stringResource(R.string.peppy_owner_computer_error_locked)
    "expired" -> stringResource(R.string.peppy_owner_computer_error_expired)
    "offline" -> stringResource(R.string.peppy_owner_computer_error_offline)
    "server-rejected" -> stringResource(R.string.peppy_owner_computer_error_server_rejected)
    "enrollment" -> stringResource(R.string.peppy_owner_computer_error_enrollment)
    else -> stringResource(R.string.peppy_owner_computer_error_generic)
}
