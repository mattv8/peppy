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
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.platform.LocalLifecycleOwner
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
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import uniffi.peppy_mobile_bindings.generateHostedPassphrase

private enum class EnrollmentStage {
    SERVER, OWNER, CLAIMED_CHOOSER, SIGNIN, MANUAL, PHONE,
    INVITE_REQUEST, INVITE_WAITING, INVITE_APPROVED, INVITE_DENIED, INVITE_EXPIRED,
}

/** Fixed mask so the hidden generated secret's length is not revealed. */
private const val MASKED_SECRET = "••••••••"
private const val MIN_ADMIN_PASSWORD_BYTES = 12
private const val MAX_ADMIN_PASSWORD_BYTES = 1024

internal fun hasValidAdminPasswordLength(password: String): Boolean =
    password.toByteArray(Charsets.UTF_8).size in MIN_ADMIN_PASSWORD_BYTES..MAX_ADMIN_PASSWORD_BYTES

internal data class AdminPasswordInputs(
    val password: String = "",
    val confirmation: String = "",
)

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

/** Tracks the one automatic transfer confirmation allowed for a phone scan. */
internal class PhoneEnrollmentAttempt {
    private var active = true
    private var automaticConfirmationStarted = false

    enum class Completion { SUCCEEDED, FAILED, IGNORED }

    @Synchronized
    fun startAutomaticConfirmation(state: EnrollmentHost.PhoneState, requiresSharedSecret: Boolean): Boolean {
        if (!active || automaticConfirmationStarted || state != EnrollmentHost.PhoneState.APPROVED || requiresSharedSecret) return false
        automaticConfirmationStarted = true
        return true
    }

    @Synchronized
    fun complete(result: ImportResult, current: Boolean): Completion {
        if (!active || !current) return Completion.IGNORED
        active = false
        return if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) Completion.SUCCEEDED else Completion.FAILED
    }

    fun deliverCompletion(
        result: ImportResult,
        current: Boolean,
        onSuccess: () -> Unit,
        onFailure: () -> Unit,
    ): Completion = complete(result, current).also { completion ->
        when (completion) {
            Completion.SUCCEEDED -> onSuccess()
            Completion.FAILED -> onFailure()
            Completion.IGNORED -> Unit
        }
    }

    @Synchronized fun cancel() {
        active = false
    }
}

internal fun canClaimOwner(
    ownerResume: Boolean,
    ownerSecretStored: Boolean,
    setupToken: String,
    setupCodeRequired: Boolean,
    adminPassword: String,
    adminPasswordConfirm: String,
    secret: String,
    busy: Boolean,
): Boolean =
    (secret.isNotBlank() || (ownerResume && ownerSecretStored)) &&
        (!setupCodeRequired || setupToken.isNotBlank()) &&
        adminPassword == adminPasswordConfirm &&
        hasValidAdminPasswordLength(adminPassword) &&
        !busy

/** The entered secret is asked for only when a resumed checkpoint has no stored pending secret (legacy). */
internal fun ownerSecretFieldVisible(ownerResume: Boolean, ownerSecretStored: Boolean): Boolean =
    ownerResume && !ownerSecretStored

/** Approved vault creation asks for the secret only when the pending slot is missing; unavailable storage never prompts. */
internal fun recoverySecretFieldVisible(state: EnrollmentHost.ResumeSecret): Boolean = state == EnrollmentHost.ResumeSecret.PROMPT

internal fun canCreateVault(
    username: String,
    adminPassword: String,
    adminPasswordConfirm: String,
    secret: String,
    state: EnrollmentHost.ResumeSecret,
    busy: Boolean,
): Boolean =
    username.isNotBlank() &&
        adminPassword == adminPasswordConfirm &&
        hasValidAdminPasswordLength(adminPassword) &&
        (secret.isNotBlank() || state == EnrollmentHost.ResumeSecret.STORED) &&
        state != EnrollmentHost.ResumeSecret.UNAVAILABLE &&
        !busy

internal fun ownerSecretErrorMessage(code: String?): Int? = when (code) {
    "wrong-passphrase" -> R.string.peppy_owner_phone_error_wrong_passphrase
    "vault-secret-mismatch" -> R.string.peppy_owner_phone_error_secret_mismatch
    else -> null
}

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
    scanner: @Composable (Modifier, (String) -> Unit, () -> Unit) -> Unit = { modifier, onCode, onUnavailable ->
        QRScanner(modifier, onCode, onUnavailable)
    },
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val host = remember(context) { EnrollmentHost(context) }
    val scanGate = remember { EnrollmentScanGate() }
    val operations = remember { EnrollmentOperationState() }
    var stage by remember { mutableStateOf(if (initialOrigin == null) EnrollmentStage.SERVER else EnrollmentStage.PHONE) }
    var origin by remember { mutableStateOf(initialOrigin.orEmpty()) }
    var setupToken by remember { mutableStateOf("") }
    var username by remember { mutableStateOf("admin") }
    var vaultOwner by remember { mutableStateOf("") }
    var vaultOwnerRequired by remember { mutableStateOf(false) }
    var inviteLink by remember { mutableStateOf("") }
    var displayName by remember { mutableStateOf("") }
    var vaultUsername by remember { mutableStateOf("") }
    var recoveryState by remember { mutableStateOf(EnrollmentHost.ResumeSecret.NONE) }
    var enrollmentForeground by remember { mutableStateOf(lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) }
    var inviteState by remember { mutableStateOf(EnrollmentHost.VaultRequestState.PENDING) }
    var inviteGeneration by remember { mutableStateOf(0) }
    var adminPasswordInputs by remember(stage) { mutableStateOf(AdminPasswordInputs()) }
    var secret by remember { mutableStateOf("") }
    var phoneSecret by remember { mutableStateOf("") }
    var ownerResume by remember { mutableStateOf(false) }
    var ownerSecretStored by remember { mutableStateOf(false) }
    var setupCodeRequired by remember { mutableStateOf(true) }
    var discoveryGeneration by remember { mutableStateOf(0) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var busyLabel by remember { mutableStateOf<Int?>(null) }
    var phone by remember { mutableStateOf<EnrollmentHost.PhoneEnrollment?>(null) }
    var phoneStatus by remember { mutableStateOf<String?>(null) }
    var phoneState by remember { mutableStateOf<EnrollmentHost.PhoneState?>(null) }
    var phoneTerminal by remember { mutableStateOf<String?>(null) }
    var sas by remember { mutableStateOf<String?>(null) }
    var phoneAttempt by remember { mutableStateOf<PhoneEnrollmentAttempt?>(null) }
    var phoneConfirming by remember { mutableStateOf(false) }
    var scannerActive by remember { mutableStateOf(true) }
    var scanGeneration by remember { mutableStateOf(0) }
    var cameraGranted by remember {
        mutableStateOf(hasCameraPermission(context))
    }
    val cameraRequest = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { cameraGranted = it }
    val networkFailed = stringResource(R.string.peppy_production_network_failed)
    val invalidQr = stringResource(R.string.peppy_production_pairing_invalid_qr)
    val pairingExpired = stringResource(R.string.peppy_production_pairing_expired)
    val denied = stringResource(R.string.peppy_hosted_join_denied)
    val sasLabel = stringResource(R.string.peppy_production_pairing_sas_label)
    val invalidSetupToken = stringResource(R.string.peppy_enrollment_error_invalid_setup_token)
    val instanceAlreadyClaimed = stringResource(R.string.peppy_enrollment_error_already_claimed)
    val cameraUnavailable = stringResource(R.string.peppy_enrollment_camera_unavailable)

    fun run(label: Int = R.string.peppy_production_pairing_connecting, block: () -> ImportResult) {
        val operation = operations.begin() ?: return
        busy = true
        busyLabel = label
        scope.launch(Dispatchers.IO) {
            val result = try { block() } catch (_: Exception) { ImportResult.NETWORK_ERROR }
            withContext(Dispatchers.Main) {
                if (!operations.complete(operation)) return@withContext
                busy = false
                busyLabel = null
                if (result == ImportResult.IMPORTED || result == ImportResult.UPDATED) onEnrolled()
                else {
                    val errorCode = host.takeErrorCode()
                    if (errorCode == "vault_owner_required") vaultOwnerRequired = true
                    error = (enrollmentErrorMessage(errorCode) ?: ownerSecretErrorMessage(errorCode))?.let(context::getString) ?: when (result) {
                    ImportResult.SETUP_TOKEN_REJECTED -> invalidSetupToken
                    ImportResult.INSTANCE_ALREADY_CLAIMED -> instanceAlreadyClaimed
                    ImportResult.SERVER_REJECTED, ImportResult.NETWORK_ERROR -> networkFailed
                    else -> importMessage(result)
                    }
                }
            }
        }
    }

    fun resetPhone() {
        operations.invalidate()
        scanGeneration += 1
        scanGate.restart()
        phoneAttempt?.cancel()
        phoneAttempt = null
        phone?.close()
        phone = null
        phoneStatus = null
        phoneState = null
        phoneTerminal = null
        sas = null
        phoneSecret = ""
        phoneConfirming = false
        busy = false
        busyLabel = null
        error = null
    }

    fun phoneClaimTerminal(failure: EnrollmentHost.PhoneClaimFailure?): String = when (failure) {
        EnrollmentHost.PhoneClaimFailure.INVALID_QR, null -> invalidQr
        EnrollmentHost.PhoneClaimFailure.NETWORK -> networkFailed
        EnrollmentHost.PhoneClaimFailure.EXPIRED -> pairingExpired
        EnrollmentHost.PhoneClaimFailure.REJECTED -> denied
    }

    DisposableEffect(host) {
        onDispose {
            operations.invalidate()
            scannerActive = false
            scanGate.cancel()
            scanGeneration += 1
            phoneAttempt?.cancel()
            phone?.close()
            host.close()
        }
    }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, _ ->
            enrollmentForeground = lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    Column(
        Modifier.fillMaxSize().safeDrawingPadding().padding(24.dp).testTag("native-enrollment-screen"),
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
                        val operation = operations.begin() ?: return@Button
                        busy = true
                        busyLabel = R.string.peppy_production_pairing_checking
                        scope.launch(Dispatchers.IO) {
                            val discovery = try { host.discover(canonical)?.let(::parseInstanceDiscovery) } catch (_: Exception) { null }
                            val resume = if (discovery?.claimed == false) host.ownerResumeSecret(canonical) else EnrollmentHost.ResumeSecret.NONE
                            val pendingRequest = discovery?.claimed == true && host.pendingVaultRequest(canonical)
                            withContext(Dispatchers.Main) {
                                if (!operations.complete(operation)) return@withContext
                                busy = false
                                busyLabel = null
                                if (generation != discoveryGeneration) Unit
                                else if (discovery == null) error = networkFailed
                                else {
                                    origin = canonical
                                    setupToken = ""
                                    setupCodeRequired = discovery.setupCodeRequired
                                    if (discovery.claimed && pendingRequest) {
                                        stage = EnrollmentStage.INVITE_WAITING
                                    } else if (resume == EnrollmentHost.ResumeSecret.UNAVAILABLE) {
                                        error = importMessage(ImportResult.STORAGE_ERROR)
                                    } else {
                                        ownerResume = resume != EnrollmentHost.ResumeSecret.NONE
                                        ownerSecretStored = resume == EnrollmentHost.ResumeSecret.STORED
                                        host.takeErrorCode()?.let { error = importMessage(ImportResult.STORAGE_ERROR) }
                                        stage = if (discovery.claimed) EnrollmentStage.CLAIMED_CHOOSER else EnrollmentStage.OWNER
                                    }
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
                OutlinedTextField(username, { username = it }, label = { Text(stringResource(R.string.peppy_enrollment_username_label)) }, modifier = Modifier.fillMaxWidth().testTag("enrollment-username"))
                OutlinedTextField(adminPasswordInputs.password, { adminPasswordInputs = adminPasswordInputs.copy(password = it) }, label = { Text(stringResource(R.string.peppy_enrollment_admin_password)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("admin-password"))
                OutlinedTextField(
                    adminPasswordInputs.confirmation,
                    { adminPasswordInputs = adminPasswordInputs.copy(confirmation = it) },
                    label = { Text(stringResource(R.string.peppy_enrollment_admin_password_confirm)) },
                    visualTransformation = PasswordVisualTransformation(),
                    isError = adminPasswordInputs.confirmation.isNotEmpty() && adminPasswordInputs.password != adminPasswordInputs.confirmation,
                    supportingText = if (adminPasswordInputs.confirmation.isNotEmpty() && adminPasswordInputs.password != adminPasswordInputs.confirmation) {
                        { Text(stringResource(R.string.peppy_admin_password_mismatch)) }
                    } else null,
                    modifier = Modifier.fillMaxWidth().testTag("admin-password-confirm"),
                )
                if (ownerSecretFieldVisible(ownerResume, ownerSecretStored)) {
                    OutlinedTextField(secret, { secret = it }, label = { Text(stringResource(R.string.peppy_passphrase_field)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("owner-resume-secret"))
                } else if (ownerResume) {
                    Text(stringResource(R.string.peppy_owner_phone_secret_saved_hint), style = MaterialTheme.typography.bodySmall)
                } else {
                    if (secret.isEmpty()) Button(onClick = { secret = generateHostedPassphrase() }, modifier = Modifier.testTag("generate-vault-secret")) { Text(stringResource(R.string.peppy_passphrase_generate_cta)) }
                }
                Text(
                    stringResource(if (setupCodeRequired) R.string.peppy_enrollment_setup_protected else R.string.peppy_enrollment_setup_open),
                    Modifier.testTag("enrollment-setup-mode").semantics { liveRegion = LiveRegionMode.Polite },
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(
                    onClick = {
                        val adminPassword = adminPasswordInputs.password
                        run { host.claimOwner(origin, username, if (setupCodeRequired) setupToken else "", adminPassword, secret) }
                    },
                    enabled = canClaimOwner(ownerResume, ownerSecretStored, setupToken, setupCodeRequired, adminPasswordInputs.password, adminPasswordInputs.confirmation, secret, busy),
                    modifier = Modifier.testTag("claim-owner"),
                ) { Text(stringResource(R.string.peppy_continue)) }
            }
            EnrollmentStage.CLAIMED_CHOOSER -> {
                Text(stringResource(R.string.peppy_enrollment_signin_body), Modifier.testTag("claimed-chooser-screen"))
                Button(onClick = { stage = EnrollmentStage.SIGNIN }, modifier = Modifier.testTag("claimed-chooser-signin")) { Text(stringResource(R.string.peppy_enrollment_signin_password_label)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.MANUAL }, modifier = Modifier.testTag("claimed-chooser-connect")) { Text(stringResource(R.string.peppy_enrollment_signin_connect_label)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.INVITE_REQUEST }, modifier = Modifier.testTag("claimed-chooser-invite")) { Text(stringResource(R.string.peppy_enrollment_signin_invite_label)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.PHONE }, modifier = Modifier.testTag("scan-phone-qr")) { Text(stringResource(R.string.peppy_scan_qr)) }
            }
            EnrollmentStage.SIGNIN -> {
                Text(stringResource(R.string.peppy_enrollment_signin_password_label))
                OutlinedTextField(username, { username = it }, label = { Text(stringResource(R.string.peppy_enrollment_username_label)) }, modifier = Modifier.fillMaxWidth().testTag("signin-username"))
                Text(stringResource(R.string.peppy_enrollment_signin_username_hint), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(adminPasswordInputs.password, { adminPasswordInputs = adminPasswordInputs.copy(password = it) }, label = { Text(stringResource(R.string.peppy_enrollment_admin_password)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("signin-password"))
                Button(onClick = {
                    val operation = operations.begin() ?: return@Button
                    val adminPassword = adminPasswordInputs.password
                    busy = true
                    busyLabel = R.string.peppy_production_pairing_connecting
                    scope.launch(Dispatchers.IO) {
                        val signedIn = try { host.signIn(origin, username, adminPassword) } catch (_: Exception) { false }
                        withContext(Dispatchers.Main) {
                            if (!operations.complete(operation)) return@withContext
                            busy = false
                            busyLabel = null
                            if (signedIn) stage = EnrollmentStage.MANUAL
                            else error = enrollmentErrorMessage(host.takeErrorCode())?.let(context::getString) ?: networkFailed
                        }
                    }
                }, enabled = adminPasswordInputs.password.isNotBlank() && !busy, modifier = Modifier.testTag("signin-submit")) { Text(stringResource(R.string.peppy_enrollment_signin_cta)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.CLAIMED_CHOOSER }, modifier = Modifier.testTag("signin-back")) { Text(stringResource(R.string.peppy_cancel)) }
            }
            EnrollmentStage.MANUAL -> {
                Text(stringResource(R.string.peppy_production_existing_vault))
                if (vaultOwnerRequired) {
                    OutlinedTextField(vaultOwner, { vaultOwner = it }, label = { Text(stringResource(R.string.peppy_enrollment_vault_owner_label)) }, modifier = Modifier.fillMaxWidth().testTag("manual-vault-owner"))
                    Text(stringResource(R.string.peppy_enrollment_vault_owner_hint), style = MaterialTheme.typography.bodySmall)
                }
                OutlinedTextField(secret, { secret = it }, label = { Text(stringResource(R.string.peppy_passphrase_field)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("manual-secret"))
                Button(onClick = { run { host.joinWithSecret(origin, vaultOwner.takeIf(String::isNotBlank), secret) } }, enabled = secret.isNotBlank() && !busy, modifier = Modifier.testTag("manual-enroll")) { Text(stringResource(R.string.peppy_continue)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.PHONE }, modifier = Modifier.testTag("scan-phone-qr")) { Text(stringResource(R.string.peppy_scan_qr)) }
            }
            EnrollmentStage.INVITE_REQUEST -> {
                Text(stringResource(R.string.peppy_enrollment_invite_request_headline), Modifier.testTag("invite-request-screen"))
                OutlinedTextField(inviteLink, { inviteLink = it }, label = { Text(stringResource(R.string.peppy_enrollment_invite_link_label)) }, modifier = Modifier.fillMaxWidth().testTag("invite-link-input"))
                OutlinedTextField(displayName, { displayName = it }, label = { Text(stringResource(R.string.peppy_enrollment_display_name_label)) }, modifier = Modifier.fillMaxWidth().testTag("display-name-input"))
                Button(onClick = {
                    val operation = operations.begin() ?: return@Button
                    val generation = ++inviteGeneration
                    busy = true
                    busyLabel = R.string.peppy_production_pairing_preparing
                    scope.launch(Dispatchers.IO) {
                        val result = try { host.beginVaultRequest(origin, inviteLink, displayName) } catch (_: Exception) { null }
                        withContext(Dispatchers.Main) {
                            if (operations.complete(operation) && generation == inviteGeneration) {
                                busy = false
                                busyLabel = null
                                if (result == null) error = enrollmentErrorMessage(host.takeErrorCode())?.let(context::getString) ?: networkFailed
                                else { inviteState = result; stage = EnrollmentStage.INVITE_WAITING; error = null }
                            }
                        }
                    }
                }, enabled = inviteLink.isNotBlank() && displayName.isNotBlank() && !busy, modifier = Modifier.testTag("invite-submit")) { Text(stringResource(R.string.peppy_enrollment_invite_request_cta)) }
                OutlinedButton(onClick = { stage = EnrollmentStage.CLAIMED_CHOOSER }, modifier = Modifier.testTag("invite-cancel")) { Text(stringResource(R.string.peppy_enrollment_invite_cancel_cta)) }
            }
            EnrollmentStage.INVITE_WAITING -> {
                Text(stringResource(R.string.peppy_enrollment_invite_waiting_headline), Modifier.testTag("invite-waiting-screen").semantics { liveRegion = LiveRegionMode.Polite })
                Text(stringResource(R.string.peppy_enrollment_invite_waiting_body))
                LaunchedEffect(stage, origin, inviteGeneration, enrollmentForeground) {
                    var backoff = 1_000L
                    while (stage == EnrollmentStage.INVITE_WAITING && enrollmentForeground) {
                        if (busy) {
                            delay(100)
                            continue
                        }
                        val observedGeneration = inviteGeneration
                        when (withContext(Dispatchers.IO) { host.vaultRequestStatus(origin, inviteState) }) {
                            EnrollmentHost.VaultRequestState.APPROVED -> if (observedGeneration == inviteGeneration && stage == EnrollmentStage.INVITE_WAITING) {
                                recoveryState = withContext(Dispatchers.IO) { host.vaultCreationSecret(origin) }
                                if (recoveryState == EnrollmentHost.ResumeSecret.UNAVAILABLE) error = importMessage(ImportResult.STORAGE_ERROR)
                                stage = EnrollmentStage.INVITE_APPROVED
                            }
                            EnrollmentHost.VaultRequestState.DENIED -> if (observedGeneration == inviteGeneration && stage == EnrollmentStage.INVITE_WAITING) stage = EnrollmentStage.INVITE_DENIED
                            EnrollmentHost.VaultRequestState.EXPIRED -> if (observedGeneration == inviteGeneration && stage == EnrollmentStage.INVITE_WAITING) stage = EnrollmentStage.INVITE_EXPIRED
                            EnrollmentHost.VaultRequestState.PENDING -> { backoff = 1_000L; inviteState = EnrollmentHost.VaultRequestState.PENDING }
                            null -> { delay(backoff); backoff = minOf(backoff * 2, 10_000L) }
                        }
                    }
                }
                OutlinedButton(onClick = {
                    val operation = operations.begin() ?: return@OutlinedButton
                    busy = true
                    busyLabel = R.string.peppy_production_pairing_cancelling
                    scope.launch(Dispatchers.IO) {
                        val cancelled = host.cancelVaultRequest(origin)
                        withContext(Dispatchers.Main) {
                            if (operations.complete(operation)) {
                                busy = false
                                busyLabel = null
                                if (cancelled) {
                                    inviteGeneration += 1
                                    stage = EnrollmentStage.CLAIMED_CHOOSER
                                } else error = networkFailed
                            }
                        }
                    }
                }, enabled = !busy, modifier = Modifier.testTag("invite-cancel")) { Text(stringResource(R.string.peppy_enrollment_invite_cancel_cta)) }
            }
            EnrollmentStage.INVITE_APPROVED -> {
                Text(stringResource(R.string.peppy_enrollment_invite_approved_headline), Modifier.testTag("invite-approved-screen").semantics { liveRegion = LiveRegionMode.Assertive })
                Text(stringResource(R.string.peppy_enrollment_invite_approved_body))
                OutlinedTextField(vaultUsername, { vaultUsername = it }, label = { Text(stringResource(R.string.peppy_enrollment_username_label)) }, modifier = Modifier.fillMaxWidth().testTag("vault-username"))
                Text(stringResource(R.string.peppy_enrollment_invite_approved_secure_hint), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(adminPasswordInputs.password, { adminPasswordInputs = adminPasswordInputs.copy(password = it) }, label = { Text(stringResource(R.string.peppy_enrollment_admin_password)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("vault-password"))
                OutlinedTextField(
                    adminPasswordInputs.confirmation,
                    { adminPasswordInputs = adminPasswordInputs.copy(confirmation = it) },
                    label = { Text(stringResource(R.string.peppy_enrollment_admin_password_confirm)) },
                    visualTransformation = PasswordVisualTransformation(),
                    isError = adminPasswordInputs.confirmation.isNotEmpty() && adminPasswordInputs.password != adminPasswordInputs.confirmation,
                    supportingText = if (adminPasswordInputs.confirmation.isNotEmpty() && adminPasswordInputs.password != adminPasswordInputs.confirmation) {
                        { Text(stringResource(R.string.peppy_admin_password_mismatch)) }
                    } else null,
                    modifier = Modifier.fillMaxWidth().testTag("vault-password-confirm"),
                )
                if (recoverySecretFieldVisible(recoveryState)) {
                    OutlinedTextField(secret, { secret = it }, label = { Text(stringResource(R.string.peppy_passphrase_field)) }, visualTransformation = PasswordVisualTransformation(), modifier = Modifier.fillMaxWidth().testTag("recovery-vault-secret"))
                } else if (recoveryState == EnrollmentHost.ResumeSecret.STORED) {
                    Text(stringResource(R.string.peppy_owner_phone_secret_saved_hint), style = MaterialTheme.typography.bodySmall)
                } else {
                    if (secret.isEmpty()) Button(onClick = { secret = generateHostedPassphrase() }, modifier = Modifier.testTag("generate-vault-secret")) { Text(stringResource(R.string.peppy_passphrase_generate_cta)) }
                }
                Button(
                    onClick = {
                        val adminPassword = adminPasswordInputs.password
                        run { host.createVault(origin, vaultUsername, adminPassword, secret) }
                    },
                    enabled = canCreateVault(vaultUsername, adminPasswordInputs.password, adminPasswordInputs.confirmation, secret, recoveryState, busy),
                    modifier = Modifier.testTag("create-vault-submit"),
                ) { Text(stringResource(R.string.peppy_enrollment_create_vault_cta)) }
            }
            EnrollmentStage.INVITE_DENIED, EnrollmentStage.INVITE_EXPIRED -> {
                val expired = stage == EnrollmentStage.INVITE_EXPIRED
                Text(stringResource(if (expired) R.string.peppy_enrollment_invite_expired_headline else R.string.peppy_enrollment_invite_denied_headline), Modifier.testTag(if (expired) "invite-expired-screen" else "invite-denied-screen").semantics { liveRegion = LiveRegionMode.Assertive })
                Text(stringResource(if (expired) R.string.peppy_enrollment_invite_expired_body else R.string.peppy_enrollment_invite_denied_body))
                OutlinedButton(onClick = {
                    val operation = operations.begin() ?: return@OutlinedButton
                    busy = true
                    busyLabel = R.string.peppy_production_pairing_cancelling
                    scope.launch(Dispatchers.IO) {
                        val cancelled = host.cancelVaultRequest(origin)
                        withContext(Dispatchers.Main) {
                            if (operations.complete(operation)) {
                                busy = false
                                busyLabel = null
                                if (cancelled) {
                                    inviteGeneration += 1
                                    stage = EnrollmentStage.CLAIMED_CHOOSER
                                } else error = networkFailed
                            }
                        }
                    }
                }, enabled = !busy, modifier = Modifier.testTag("invite-restart")) { Text(stringResource(R.string.peppy_enrollment_invite_restart_cta)) }
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
                        scanner(Modifier.fillMaxWidth().weight(1f).testTag("enrollment-qr-scanner"), { raw ->
                            if (scannerActive && !busy && phone == null && phoneTerminal == null && scanGate.tryStart()) {
                                val operation = operations.begin() ?: return@scanner
                                busy = true
                                busyLabel = R.string.peppy_production_pairing_connecting
                                val generation = scanGeneration
                                scope.launch(Dispatchers.IO) {
                                    var accepted = false
                                    var candidate: EnrollmentHost.PhoneEnrollment? = null
                                    try {
                                        val inviteOrigin = host.parseVaultInviteLink(raw)
                                        if (inviteOrigin == null) candidate = host.beginPhoneClaim(raw)
                                        withContext(Dispatchers.Main) {
                                            if (operations.isActive(operation) && scannerActive && generation == scanGeneration && phone == null && scanGate.acceptResult()) {
                                                if (inviteOrigin != null) {
                                                    inviteLink = raw
                                                    origin = inviteOrigin
                                                    stage = EnrollmentStage.INVITE_REQUEST
                                                    scanGate.finish()
                                                    accepted = true
                                                }
                                                if (candidate == null && !accepted) {
                                                    phoneTerminal = phoneClaimTerminal(host.takePhoneClaimFailure())
                                                } else if (!accepted) {
                                                    phone = candidate
                                                    phoneAttempt = PhoneEnrollmentAttempt()
                                                    accepted = true
                                                    error = null
                                                }
                                            }
                                        }
                                    } catch (_: Exception) {
                                        withContext(Dispatchers.Main) {
                                            if (operations.isActive(operation) && scannerActive && generation == scanGeneration && phone == null && scanGate.acceptResult()) {
                                                phoneTerminal = invalidQr
                                            }
                                        }
                                    } finally {
                                        if (!accepted) candidate?.close()
                                        withContext(NonCancellable + Dispatchers.Main) {
                                            if (operations.complete(operation)) {
                                                busy = false
                                                busyLabel = null
                                            }
                                        }
                                    }
                                }
                            }
                        }, { phoneTerminal = cameraUnavailable })
                        if (busy) CircularProgressIndicator(Modifier.testTag("phone-scan-waiting"))
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
                                        if (sas == null) {
                                            phoneTerminal = invalidQr
                                            flow.close()
                                            return@LaunchedEffect
                                        }
                                        val attempt = phoneAttempt
                                        if (attempt?.startAutomaticConfirmation(currentState, flow.requiresSharedSecret()) == true) {
                                            val generation = scanGeneration
                                            phoneConfirming = true
                                            val result = withContext(Dispatchers.IO) { flow.confirm(current, null) }
                                            attempt.deliverCompletion(
                                                result = result,
                                                current = scannerActive && generation == scanGeneration && phone === flow,
                                                onSuccess = {
                                                    phoneConfirming = false
                                                    onEnrolled()
                                                },
                                                onFailure = {
                                                    phoneConfirming = false
                                                    phoneTerminal = when (result) {
                                                        ImportResult.NETWORK_ERROR, ImportResult.SERVER_REJECTED -> networkFailed
                                                        else -> importMessage(result)
                                                    }
                                                    flow.close()
                                                },
                                            )
                                        }
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
                        if (flow.requiresSharedSecret()) {
                            OutlinedTextField(
                                phoneSecret,
                                { phoneSecret = it },
                                label = { Text(stringResource(R.string.peppy_passphrase_field)) },
                                visualTransformation = PasswordVisualTransformation(),
                                modifier = Modifier.fillMaxWidth().testTag("phone-shared-secret"),
                            )
                            Button(
                                onClick = { phoneStatus?.let { status -> run { flow.confirm(status, phoneSecret.takeIf(String::isNotBlank)) } } },
                                enabled = phoneState == EnrollmentHost.PhoneState.APPROVED && phoneSecret.isNotBlank() && !busy,
                                modifier = Modifier.testTag("phone-confirm"),
                            ) { Text(stringResource(R.string.peppy_continue)) }
                        } else if (phoneConfirming && phoneState == EnrollmentHost.PhoneState.APPROVED) {
                            Column(
                                Modifier.testTag("phone-connecting"),
                                horizontalAlignment = Alignment.CenterHorizontally,
                                verticalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                CircularProgressIndicator()
                                Text(
                                    stringResource(R.string.peppy_production_pairing_connecting),
                                    Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
                                )
                            }
                        }
                    }
                }
            }
        }
        error?.let { Text(it, Modifier.testTag("enrollment-error").semantics { liveRegion = LiveRegionMode.Assertive }) }
        busyLabel?.let { label ->
            Row(
                Modifier.testTag("enrollment-progress").semantics { liveRegion = LiveRegionMode.Polite },
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                CircularProgressIndicator(Modifier.testTag("enrollment-progress-spinner"))
                Text(stringResource(label))
            }
        }
        OutlinedButton(onClick = {
            adminPasswordInputs = AdminPasswordInputs()
            inviteGeneration += 1
            scannerActive = false
            scanGate.cancel()
            scanGeneration += 1
            phoneAttempt?.cancel()
            phone?.close()
            host.close()
            operations.invalidate()
            busy = false
            busyLabel = null
            onDismiss()
        }, enabled = !phoneConfirming && !busy, modifier = Modifier.testTag("enrollment-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
    }
}
