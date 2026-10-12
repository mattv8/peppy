package dev.peppy.mobile

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.generateHostedPassphrase

private enum class EnrollmentStage { SERVER, OWNER, MANUAL, PHONE }

/** Fixed mask so the hidden generated secret's length is not revealed. */
private const val MASKED_SECRET = "••••••••"

internal class EnrollmentScanGate {
    private var active = true
    private var inFlight = false

    @Synchronized fun tryStart(): Boolean {
        if (!active || inFlight) return false
        inFlight = true
        return true
    }

    @Synchronized fun acceptResult(): Boolean = active && inFlight
    @Synchronized fun finish() { inFlight = false }
    @Synchronized fun restart() { active = true; inFlight = false }
    @Synchronized fun cancel() { active = false; inFlight = false }
}

internal fun canClaimOwner(
    ownerResume: Boolean,
    setupToken: String,
    setupCodeRequired: Boolean,
    adminPassword: String,
    secret: String,
    saved: Boolean,
    busy: Boolean,
): Boolean =
        secret.isNotBlank() &&
        (ownerResume || saved) &&
        (!setupCodeRequired || setupToken.isNotBlank()) &&
        adminPassword.length >= 12 &&
        !busy

@Composable
internal fun GeneratedSecretControls(
    secret: String,
    revealSecret: Boolean,
    saved: Boolean,
    onToggleReveal: () -> Unit,
    onAcknowledge: (Boolean) -> Unit,
) {
    if (revealSecret) Text(secret, Modifier.testTag("vault-secret"))
    else Text(MASKED_SECRET, Modifier.testTag("vault-secret-masked"))
    OutlinedButton(onClick = onToggleReveal, modifier = Modifier.testTag("vault-secret-reveal")) {
        Text(stringResource(if (revealSecret) R.string.peppy_passphrase_hide else R.string.peppy_passphrase_show))
    }
    Row(
        Modifier.fillMaxWidth().testTag("vault-secret-ack").toggleable(saved, role = Role.Checkbox, onValueChange = onAcknowledge),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Checkbox(saved, null)
        Text(stringResource(R.string.peppy_passphrase_ack_label))
    }
}

/** Native enrollment keeps passwords and the vault secret in Compose state only. */
@Composable
internal fun EnrollmentScreen(
    initialOrigin: String? = null,
    onDismiss: () -> Unit,
    onEnrolled: () -> Unit,
    hasCameraPermission: (android.content.Context) -> Boolean = { context ->
        context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
    },
    scanner: @Composable (Modifier, (String) -> Unit) -> Unit = { modifier, onCode ->
        QRScanner(modifier, onCode)
    },
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val host = remember(context) { EnrollmentHost(context) }
    val scanGate = remember { EnrollmentScanGate() }
    var stage by remember { mutableStateOf(if (initialOrigin == null) EnrollmentStage.SERVER else EnrollmentStage.PHONE) }
    var origin by remember { mutableStateOf(initialOrigin.orEmpty()) }
    var setupToken by remember { mutableStateOf("") }
    var adminPassword by remember { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    var revealSecret by remember { mutableStateOf(false) }
    var phoneSecret by remember { mutableStateOf("") }
    var saved by remember { mutableStateOf(false) }
    var ownerResume by remember { mutableStateOf(false) }
    var setupCodeRequired by remember { mutableStateOf(true) }
    var discoveryGeneration by remember { mutableStateOf(0) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var phone by remember { mutableStateOf<EnrollmentHost.PhoneEnrollment?>(null) }
    var phoneStatus by remember { mutableStateOf<String?>(null) }
    var phoneState by remember { mutableStateOf<EnrollmentHost.PhoneState?>(null) }
    var phoneTerminal by remember { mutableStateOf<String?>(null) }
    var sas by remember { mutableStateOf<String?>(null) }
    var confirmed by remember { mutableStateOf(false) }
    var scannerActive by remember { mutableStateOf(true) }
    var scanGeneration by remember { mutableStateOf(0) }
    var cameraGranted by remember {
        mutableStateOf(hasCameraPermission(context))
    }
    val cameraRequest = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { cameraGranted = it }
    val networkFailed = stringResource(R.string.peppy_production_network_failed)
    val invalidQr = stringResource(R.string.peppy_production_pairing_invalid_qr)
    val denied = stringResource(R.string.peppy_hosted_join_denied)
    val sasLabel = stringResource(R.string.peppy_production_pairing_sas_label)
    val invalidSetupToken = stringResource(R.string.peppy_enrollment_error_invalid_setup_token)
    val instanceAlreadyClaimed = stringResource(R.string.peppy_enrollment_error_already_claimed)

    fun run(block: () -> ImportResult) {
        if (busy) return
        busy = true
        scope.launch(Dispatchers.IO) {
            val result = try { block() } catch (_: Exception) { ImportResult.NETWORK_ERROR }
            withContext(Dispatchers.Main) {
                busy = false
                if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) onEnrolled()
                else error = when (result) {
                    ImportResult.SETUP_TOKEN_REJECTED -> invalidSetupToken
                    ImportResult.INSTANCE_ALREADY_CLAIMED -> instanceAlreadyClaimed
                    ImportResult.SERVER_REJECTED, ImportResult.NETWORK_ERROR -> networkFailed
                    else -> importMessage(result)
                }
            }
        }
    }

    fun resetPhone() {
        scanGeneration += 1
        scanGate.restart()
        phone?.close()
        phone = null
        phoneStatus = null
        phoneState = null
        phoneTerminal = null
        sas = null
        confirmed = false
        phoneSecret = ""
        busy = false
        error = null
    }

    DisposableEffect(host) {
        onDispose {
            scannerActive = false
            scanGate.cancel()
            scanGeneration += 1
            phone?.close()
            host.close()
        }
    }

    Column(
        Modifier.fillMaxSize().padding(24.dp).testTag("native-enrollment-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        when (stage) {
            EnrollmentStage.SERVER -> {
                Text(stringResource(R.string.peppy_self_hosted_headline))
                OutlinedTextField(
                    origin,
                    { origin = it; discoveryGeneration += 1; error = null },
                    label = { Text(stringResource(R.string.peppy_settings_server_origin)) },
                    modifier = Modifier.fillMaxWidth().testTag("enrollment-origin"),
                )
                Button(onClick = {
                    val generation = discoveryGeneration
                    val canonical = CredentialParser.canonicalOrigin(origin)
                    if (canonical == null) {
                        error = networkFailed
                    } else {
                        busy = true
                        scope.launch(Dispatchers.IO) {
                            val discovery = try { host.discover(canonical)?.let(::parseInstanceDiscovery) } catch (_: Exception) { null }
                            withContext(Dispatchers.Main) {
                                busy = false
                                if (generation != discoveryGeneration) Unit
                                else if (discovery == null) error = networkFailed
                                else {
                                    origin = canonical
                                    setupToken = ""
                                    setupCodeRequired = discovery.setupCodeRequired
                                    ownerResume = !discovery.claimed && host.hasPendingOwner(canonical)
                                    stage = if (discovery.claimed) EnrollmentStage.MANUAL else EnrollmentStage.OWNER
                                }
                            }
                        }
                    }
                }, enabled = !busy, modifier = Modifier.testTag("enrollment-discover")) {
                    Text(stringResource(R.string.peppy_continue))
                }
                OutlinedButton(onClick = { error = null; stage = EnrollmentStage.PHONE }, modifier = Modifier.testTag("scan-phone-qr")) {
                    Text(stringResource(R.string.peppy_scan_qr))
                }
            }
            EnrollmentStage.OWNER -> {
                Text(stringResource(R.string.peppy_provisioning_headline))
                if (setupCodeRequired) {
                    OutlinedTextField(setupToken, { setupToken = it }, label = { Text(stringResource(R.string.peppy_enrollment_setup_code)) }, modifier = Modifier.fillMaxWidth().testTag("setup-token"))
                }
                OutlinedTextField(adminPassword, { adminPassword = it }, label = { Text(stringResource(R.string.peppy_enrollment_admin_password)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("admin-password"))
                if (ownerResume) {
                    OutlinedTextField(secret, { secret = it }, label = { Text(stringResource(R.string.peppy_passphrase_field)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("owner-resume-secret"))
                } else {
                    if (secret.isEmpty()) Button(onClick = { secret = generateHostedPassphrase() }, modifier = Modifier.testTag("generate-vault-secret")) { Text(stringResource(R.string.peppy_passphrase_generate_cta)) }
                    if (secret.isNotEmpty()) {
                        GeneratedSecretControls(
                            secret = secret,
                            revealSecret = revealSecret,
                            saved = saved,
                            onToggleReveal = { revealSecret = !revealSecret },
                            onAcknowledge = { saved = it },
                        )
                    }
                }
                Text(
                    stringResource(if (setupCodeRequired) R.string.peppy_enrollment_setup_protected else R.string.peppy_enrollment_setup_open),
                    Modifier.testTag("enrollment-setup-mode").semantics { liveRegion = LiveRegionMode.Polite },
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(
                    onClick = { run { host.claimOwner(origin, if (setupCodeRequired) setupToken else "", adminPassword, secret) } },
                    enabled = canClaimOwner(ownerResume, setupToken, setupCodeRequired, adminPassword, secret, saved, busy),
                    modifier = Modifier.testTag("claim-owner"),
                ) { Text(stringResource(R.string.peppy_continue)) }
            }
            EnrollmentStage.MANUAL -> {
                Text(stringResource(R.string.peppy_production_existing_vault))
                OutlinedTextField(secret, { secret = it }, label = { Text(stringResource(R.string.peppy_passphrase_field)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("manual-secret"))
                Button(onClick = { run { host.joinWithSecret(origin, secret) } }, enabled = secret.isNotBlank() && !busy, modifier = Modifier.testTag("manual-enroll")) { Text(stringResource(R.string.peppy_continue)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.PHONE }, modifier = Modifier.testTag("scan-phone-qr")) { Text(stringResource(R.string.peppy_scan_qr)) }
            }
            EnrollmentStage.PHONE -> {
                Text(stringResource(R.string.peppy_hosted_join_headline))
                val terminal = phoneTerminal
                val flow = phone
                if (terminal != null) {
                    Text(terminal, Modifier.testTag("phone-terminal-error").semantics { liveRegion = LiveRegionMode.Assertive })
                    Button(onClick = { resetPhone() }, modifier = Modifier.testTag("phone-retry")) { Text(stringResource(R.string.peppy_try_again)) }
                } else if (flow == null) {
                    if (cameraGranted) {
                        scanner(Modifier.fillMaxWidth().testTag("enrollment-qr-scanner")) { raw ->
                            if (scannerActive && !busy && phone == null && phoneTerminal == null && scanGate.tryStart()) {
                                busy = true
                                val generation = scanGeneration
                                scope.launch(Dispatchers.IO) {
                                    var accepted = false
                                    var candidate: EnrollmentHost.PhoneEnrollment? = null
                                    try {
                                        candidate = host.beginPhoneClaim(raw)
                                        withContext(Dispatchers.Main) {
                                            if (scannerActive && generation == scanGeneration && phone == null && scanGate.acceptResult()) {
                                                if (candidate == null) {
                                                    error = invalidQr
                                                    scanGate.finish()
                                                } else {
                                                    phone = candidate
                                                    accepted = true
                                                    error = null
                                                }
                                            }
                                        }
                                    } finally {
                                        if (!accepted) candidate?.close()
                                        withContext(NonCancellable + Dispatchers.Main) {
                                            if (generation == scanGeneration) busy = false
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        Button(onClick = { cameraRequest.launch(Manifest.permission.CAMERA) }, modifier = Modifier.testTag("enrollment-camera-request")) { Text(stringResource(R.string.peppy_enrollment_allow_camera)) }
                    }
                } else {
                    LaunchedEffect(flow) {
                        var networkFailures = 0
                        repeat(150) {
                            val current = withContext(Dispatchers.IO) { flow.status() }
                            if (current == null) {
                                networkFailures += 1
                                if (networkFailures >= 15) {
                                    phoneTerminal = networkFailed
                                    flow.close()
                                    return@LaunchedEffect
                                }
                            } else {
                                networkFailures = 0
                                when (val currentState = flow.state(current)) {
                                    EnrollmentHost.PhoneState.CLAIMED -> {
                                        phoneStatus = current
                                        phoneState = currentState
                                        if (sas == null) sas = withContext(Dispatchers.IO) { flow.sas(current) }
                                        if (sas == null) {
                                            phoneTerminal = invalidQr
                                            flow.close()
                                            return@LaunchedEffect
                                        }
                                    }
                                    EnrollmentHost.PhoneState.APPROVED -> {
                                        phoneStatus = current
                                        phoneState = currentState
                                        if (sas == null) sas = withContext(Dispatchers.IO) { flow.sas(current) }
                                        return@LaunchedEffect
                                    }
                                    EnrollmentHost.PhoneState.DENIED, EnrollmentHost.PhoneState.EXPIRED -> {
                                        phoneTerminal = denied
                                        flow.close()
                                        return@LaunchedEffect
                                    }
                                    EnrollmentHost.PhoneState.PENDING -> Unit
                                    null -> {
                                        phoneTerminal = invalidQr
                                        flow.close()
                                        return@LaunchedEffect
                                    }
                                }
                            }
                            delay(2_000)
                        }
                        phoneTerminal = networkFailed
                        flow.close()
                    }
                    val code = sas
                    if (code == null) {
                        Text(stringResource(R.string.peppy_hosted_join_waiting), Modifier.testTag("phone-approval-waiting"))
                    } else {
                        Text(
                            code,
                            Modifier.testTag("phone-sas").semantics { contentDescription = "$sasLabel $code" },
                            style = MaterialTheme.typography.headlineMedium,
                        )
                        if (phoneState != EnrollmentHost.PhoneState.APPROVED) {
                            Text(stringResource(R.string.peppy_hosted_join_waiting), Modifier.testTag("phone-approval-waiting"))
                        }
                        Checkbox(confirmed, { confirmed = it }, Modifier.testTag("phone-sas-confirm"))
                        Text(stringResource(R.string.peppy_production_pairing_verify))
                        if (flow.requiresSharedSecret()) {
                            OutlinedTextField(
                                phoneSecret,
                                { phoneSecret = it },
                                label = { Text(stringResource(R.string.peppy_passphrase_field)) },
                                visualTransformation = PasswordVisualTransformation(),
                                modifier = Modifier.fillMaxWidth().testTag("phone-shared-secret"),
                            )
                        }
                        Button(
                            onClick = { phoneStatus?.let { status -> run { flow.confirm(status, phoneSecret.takeIf(String::isNotBlank)) } } },
                            enabled = phoneState == EnrollmentHost.PhoneState.APPROVED && confirmed && (!flow.requiresSharedSecret() || phoneSecret.isNotBlank()) && !busy,
                            modifier = Modifier.testTag("phone-confirm"),
                        ) { Text(stringResource(R.string.peppy_continue)) }
                    }
                }
            }
        }
        error?.let { Text(it, Modifier.testTag("enrollment-error").semantics { liveRegion = LiveRegionMode.Assertive }) }
        OutlinedButton(onClick = {
            scannerActive = false
            scanGate.cancel()
            scanGeneration += 1
            phone?.close()
            host.close()
            onDismiss()
        }, modifier = Modifier.testTag("enrollment-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
    }
}
