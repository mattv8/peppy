package dev.peppy.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.MultiFormatWriter
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

private fun ownerQr(value: String): Bitmap {
    val matrix = MultiFormatWriter().encode(value, BarcodeFormat.QR_CODE, 512, 512)
    return Bitmap.createBitmap(512, 512, Bitmap.Config.ARGB_8888).also { bitmap ->
        for (y in 0 until 512) for (x in 0 until 512) bitmap.setPixel(x, y, if (matrix[x, y]) android.graphics.Color.BLACK else android.graphics.Color.WHITE)
    }
}

private fun roleLabel(requestedRole: String): Int = when (requestedRole) {
    "device" -> R.string.peppy_production_pairing_role_device
    "gateway" -> R.string.peppy_production_pairing_role_gateway
    else -> R.string.peppy_production_pairing_role_device
}

private fun computerRoleLabel(requestedRole: String): Int = when (requestedRole) {
    "device" -> R.string.peppy_production_pairing_role_computer
    else -> roleLabel(requestedRole)
}

/** Owner-only pairing UI. Network and credentials remain inside [OwnerPairingHost]. */
@Composable
internal fun OwnerPairingSheet(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val host = remember(context) { OwnerPairingHost.production(context) }
    val lifecycle = LocalLifecycleOwner.current
    var foreground by remember { mutableStateOf(lifecycle.lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.RESUMED)) }
    var intent by remember { mutableStateOf<OwnerPairingIntent?>(null) }
    var claim by remember { mutableStateOf<OwnerPairingClaim?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var confirmed by remember { mutableStateOf(false) }
    var approvalInFlight by remember { mutableStateOf(false) }
    var createAttempt by remember { mutableStateOf(0) }
    var addingComputer by remember { mutableStateOf(false) }
    var approvalJob by remember { mutableStateOf<Job?>(null) }
    val scope = rememberCoroutineScope()

    DisposableEffect(lifecycle) {
        val observer = object : DefaultLifecycleObserver {
            override fun onResume(owner: LifecycleOwner) { foreground = true }
            override fun onPause(owner: LifecycleOwner) { foreground = false; approvalJob?.cancel(); claim = null; confirmed = false }
        }
        lifecycle.lifecycle.addObserver(observer)
        onDispose { lifecycle.lifecycle.removeObserver(observer); approvalJob?.cancel() }
    }
    LaunchedEffect(createAttempt, foreground, addingComputer) {
        if (intent != null || !foreground || addingComputer) return@LaunchedEffect
        intent = withContext(Dispatchers.IO) { host.create() }
        if (intent == null) error = context.getString(R.string.peppy_production_pairing_error)
    }
    LaunchedEffect(intent?.token, foreground, addingComputer) {
        val active = intent ?: return@LaunchedEffect
        if (!foreground || addingComputer) return@LaunchedEffect
        while (System.currentTimeMillis() < active.expiresAtMs) {
            val discovered = withContext(Dispatchers.IO) { host.status(active) }
            // Any changed/disappeared claimant invalidates a prior explicit confirmation.
            if (discovered != claim) { claim = discovered; confirmed = false }
            if (discovered != null) return@LaunchedEffect
            delay(minOf(5_000, (active.expiresAtMs - System.currentTimeMillis()).coerceAtLeast(1)))
        }
    }
    LaunchedEffect(intent?.token) {
        val active = intent ?: return@LaunchedEffect
        delay((active.expiresAtMs - System.currentTimeMillis()).coerceIn(1, 900_000))
        if (intent === active) {
            confirmed = false
            error = context.getString(R.string.peppy_production_pairing_expired)
        }
    }
    if (addingComputer) {
        AddComputerSheet(host = host, onDismiss = { addingComputer = false })
        return
    }
    Column(Modifier.fillMaxWidth().testTag("owner-add-device-sheet"), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        when {
            intent == null && error == null -> CircularProgressIndicator()
            claim == null && intent != null -> {
                Text(stringResource(R.string.peppy_production_pairing_qr_body))
                val qrContent = remember(intent!!.token) {
                    ownerQr(JSONObject().put("https_origin", intent!!.origin).put("intent_token", intent!!.token).toString())
                }
                Image(qrContent.asImageBitmap(), contentDescription = stringResource(R.string.peppy_production_pairing_qr_accessibility), Modifier.testTag("owner-pairing-qr"))
                Text(stringResource(R.string.peppy_production_pairing_waiting), Modifier.testTag("owner-pairing-waiting"))
            }
            claim != null -> {
                Text(stringResource(R.string.peppy_production_pairing_role_label))
                Text(stringResource(roleLabel(claim!!.requestedRole)), Modifier.testTag("owner-pairing-role"))
                Text(stringResource(R.string.peppy_production_owner_sas_heading))
                Text(claim!!.sas, Modifier.testTag("owner-sas-code"))
                Checkbox(confirmed, { confirmed = it }, Modifier.testTag("owner-sas-confirmation"))
                Text(stringResource(R.string.peppy_production_pairing_verify))
                Button(onClick = {
                    val active = intent ?: return@Button; val approved = claim ?: return@Button
                    if (System.currentTimeMillis() >= active.expiresAtMs) {
                        confirmed = false
                        error = context.getString(R.string.peppy_production_pairing_expired)
                        return@Button
                    }
                    if (approvalInFlight || !foreground || !confirmed) return@Button
                    approvalInFlight = true
                    approvalJob = scope.launch {
                        try {
                            val approvalContext = currentCoroutineContext()
                            val ok = withContext(Dispatchers.IO) { host.approve(active, approved, true, canContinue = { approvalContext.isActive }) }
                            if (ok) onDismiss() else { confirmed = false; error = context.getString(R.string.peppy_production_pairing_error) }
                        } finally { approvalInFlight = false }
                    }
                }, enabled = confirmed && foreground && !approvalInFlight && error == null, modifier = Modifier.testTag("owner-approve-button")) { Text(stringResource(R.string.peppy_device_allow)) }
            }
        }
        if (error != null) {
            Text(error!!, Modifier.testTag("owner-pairing-error"))
            Button(onClick = { intent = null; claim = null; error = null; confirmed = false; createAttempt += 1 }, enabled = !approvalInFlight, modifier = Modifier.testTag("owner-retry-button")) {
                Text(stringResource(R.string.peppy_production_pairing_retry))
            }
        }
        OutlinedButton(onClick = { confirmed = false; addingComputer = true }, enabled = !approvalInFlight && foreground, modifier = Modifier.testTag("add-computer-button")) { Text(stringResource(R.string.peppy_production_add_computer)) }
        OutlinedButton(onClick = onDismiss, Modifier.testTag("owner-pairing-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
    }
}

private sealed interface AddComputerState {
    data object Scanning : AddComputerState
    data class Offering(val payload: String) : AddComputerState
    data class Waiting(val intent: OwnerPairingIntent) : AddComputerState
    data class Claimed(val intent: OwnerPairingIntent, val claim: OwnerPairingClaim) : AddComputerState
    data class Error(val error: OwnerPairingComputerError) : AddComputerState
}

private fun computerError(context: android.content.Context, error: OwnerPairingComputerError): String = context.getString(when (error) {
    OwnerPairingComputerError.ORIGIN_MISMATCH -> R.string.peppy_production_add_computer_origin_mismatch
    OwnerPairingComputerError.EXPIRED -> R.string.peppy_production_pairing_expired
    OwnerPairingComputerError.ALREADY_LINKED -> R.string.peppy_production_add_computer_already_linked
    OwnerPairingComputerError.PAIRING -> R.string.peppy_production_pairing_error
    OwnerPairingComputerError.NETWORK -> R.string.peppy_production_network_failed
    OwnerPairingComputerError.PAUSED -> R.string.peppy_production_pairing_paused
})

@Composable
private fun AddComputerSheet(host: OwnerPairingHost, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current
    var foreground by remember { mutableStateOf(lifecycle.lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.RESUMED)) }
    var state by remember { mutableStateOf<AddComputerState>(AddComputerState.Scanning) }
    var confirmed by remember { mutableStateOf(false) }
    var approving by remember { mutableStateOf(false) }
    var approvalJob by remember { mutableStateOf<Job?>(null) }
    val scope = rememberCoroutineScope()

    DisposableEffect(lifecycle) {
        val observer = object : androidx.lifecycle.DefaultLifecycleObserver {
            override fun onResume(owner: LifecycleOwner) { foreground = true }
            override fun onPause(owner: LifecycleOwner) {
                foreground = false
                confirmed = false
                approvalJob?.cancel()
                state = AddComputerState.Error(OwnerPairingComputerError.PAUSED)
            }
        }
        lifecycle.lifecycle.addObserver(observer)
        onDispose { lifecycle.lifecycle.removeObserver(observer); approvalJob?.cancel() }
    }

    LaunchedEffect(state, foreground) {
        val offering = state as? AddComputerState.Offering ?: return@LaunchedEffect
        if (!foreground) return@LaunchedEffect
        when (val result = withContext(Dispatchers.IO) { host.startComputerPairing(offering.payload) }) {
            is OwnerPairingComputerResult.Ready -> state = AddComputerState.Waiting(result.intent)
            is OwnerPairingComputerResult.Error -> state = AddComputerState.Error(result.error)
        }
    }
    LaunchedEffect(state, foreground) {
        val waiting = state as? AddComputerState.Waiting ?: return@LaunchedEffect
        if (!foreground) return@LaunchedEffect
        while (System.currentTimeMillis() < waiting.intent.expiresAtMs) {
            val claim = withContext(Dispatchers.IO) { host.status(waiting.intent) }
            if (claim != null) {
                if (claim.requestedRole != "device") {
                    state = AddComputerState.Error(OwnerPairingComputerError.PAIRING)
                    return@LaunchedEffect
                }
                state = AddComputerState.Claimed(waiting.intent, claim)
                return@LaunchedEffect
            }
            delay(minOf(5_000, (waiting.intent.expiresAtMs - System.currentTimeMillis()).coerceAtLeast(1)))
        }
        state = AddComputerState.Error(OwnerPairingComputerError.EXPIRED)
    }
    val activeIntent = when (val current = state) {
        is AddComputerState.Waiting -> current.intent
        is AddComputerState.Claimed -> current.intent
        else -> null
    }
    LaunchedEffect(activeIntent?.token) {
        val active = activeIntent ?: return@LaunchedEffect
        delay((active.expiresAtMs - System.currentTimeMillis()).coerceIn(1, 900_000))
        confirmed = false
        approvalJob?.cancel()
        state = AddComputerState.Error(OwnerPairingComputerError.EXPIRED)
    }

    Column(Modifier.fillMaxWidth().testTag("add-computer-sheet"), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        when (val current = state) {
            AddComputerState.Scanning -> {
                if (foreground) QRScanner(Modifier.fillMaxWidth().testTag("add-computer-scanner")) { raw ->
                    if (foreground && state == AddComputerState.Scanning) state = AddComputerState.Offering(raw)
                }
                Text(stringResource(R.string.peppy_production_add_computer_scan_body))
            }
            is AddComputerState.Offering, is AddComputerState.Waiting -> CircularProgressIndicator()
            is AddComputerState.Claimed -> {
                Text(stringResource(R.string.peppy_production_pairing_role_label))
                Text(stringResource(computerRoleLabel(current.claim.requestedRole)))
                Text(stringResource(R.string.peppy_production_owner_sas_heading))
                Text(current.claim.sas, Modifier.testTag("add-computer-sas-code"))
                Checkbox(confirmed, { confirmed = it }, Modifier.testTag("add-computer-sas-confirm"))
                Text(stringResource(R.string.peppy_production_pairing_verify))
                Button(onClick = {
                    if (System.currentTimeMillis() >= current.intent.expiresAtMs) {
                        confirmed = false
                        state = AddComputerState.Error(OwnerPairingComputerError.EXPIRED)
                        return@Button
                    }
                    if (!confirmed || approving || !foreground) return@Button
                    approving = true
                    approvalJob = scope.launch {
                        try {
                            val approvalContext = currentCoroutineContext()
                            if (withContext(Dispatchers.IO) { host.approve(current.intent, current.claim, true, expectedRole = "device", canContinue = { approvalContext.isActive }) }) onDismiss()
                            else state = AddComputerState.Error(OwnerPairingComputerError.PAIRING)
                        } finally { approving = false }
                    }
                }, enabled = confirmed && !approving && foreground && System.currentTimeMillis() < current.intent.expiresAtMs, modifier = Modifier.testTag("add-computer-allow-button")) { Text(stringResource(R.string.peppy_device_allow)) }
                OutlinedButton(onClick = { confirmed = false; state = AddComputerState.Scanning }, enabled = !approving && foreground, modifier = Modifier.testTag("add-computer-deny-button")) { Text(stringResource(R.string.peppy_device_deny)) }
            }
            is AddComputerState.Error -> {
                Text(computerError(context, current.error), Modifier.testTag("add-computer-error"))
                Button(onClick = { confirmed = false; state = AddComputerState.Scanning }, enabled = foreground && !approving) { Text(stringResource(R.string.peppy_production_pairing_retry)) }
            }
        }
        OutlinedButton(onClick = onDismiss, modifier = Modifier.testTag("add-computer-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
    }
}
