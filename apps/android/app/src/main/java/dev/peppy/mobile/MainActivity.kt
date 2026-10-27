package dev.peppy.mobile

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Contacts
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Sms
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.ui.theme.PeppyTheme
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.ScreenBody
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val systemBars = SystemBarStyle.auto(Color.TRANSPARENT, Color.TRANSPARENT)
        enableEdgeToEdge(statusBarStyle = systemBars, navigationBarStyle = systemBars)
        setContent { PeppyTheme { Surface { CompanionScreen(this@MainActivity) } } }
    }

    override fun onResume() {
        super.onResume()
        // A challenge can expire while the process remains alive; foreground re-enters the
        // serialized relay reconciliation path without doing network work on the activity.
        RelayWakeRuntime.enrollmentChanged(applicationContext)
        SmsCaptureLifecycle.onPermissionOrForeground(applicationContext)
        ContactSyncHost.onPermissionOrForeground(applicationContext)
    }
}

internal val SMS_PERMISSIONS = arrayOf(Manifest.permission.RECEIVE_SMS, Manifest.permission.SEND_SMS, Manifest.permission.READ_SMS)
internal val MMS_PERMISSIONS = arrayOf(Manifest.permission.READ_SMS, Manifest.permission.RECEIVE_MMS)

internal fun granted(context: Context, permission: String) =
    context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

internal fun importMessage(result: ImportResult): String = when (result) {
    ImportResult.IMPORTED -> "Credential verified for this phone. Unlock with the vault passphrase next."
    ImportResult.UPDATED -> "Credential refreshed for the same device. Local data and its protected key were kept."
    ImportResult.INVALID_CREDENTIAL -> "This file is not a valid Peppy v1 device credential (16 KiB maximum)."
    ImportResult.SERVER_REJECTED -> "The server did not accept this credential for its vault and device."
    ImportResult.SETUP_TOKEN_REJECTED -> "The setup code is incorrect. Check the code configured with PEPPY_SETUP_TOKEN and try again."
    ImportResult.INSTANCE_ALREADY_CLAIMED -> "This server has already been set up. Connect with your admin password or vault shared secret instead."
    ImportResult.NOT_A_GATEWAY -> "This credential belongs to a viewing device. Pair this phone to send SMS instead."
    ImportResult.NETWORK_ERROR -> "Could not reach the Peppy server. Check the connection and try again."
    ImportResult.IDENTITY_MISMATCH -> "This phone is already bound to a different server, vault, or device. Import refused to protect existing data."
    ImportResult.EXISTING_DATA_WITHOUT_KEY -> "Local Peppy data exists without its protected key. Import refused so that data is not overwritten."
    ImportResult.STORAGE_ERROR -> "Secure storage on this phone is unavailable. Nothing was saved."
}

internal fun enrollmentErrorMessage(code: String?): Int? = when (code) {
    "admin_required" -> R.string.peppy_enrollment_error_admin_required
    "foreign_vault" -> R.string.peppy_enrollment_error_foreign_vault
    "vault_owner_required" -> R.string.peppy_enrollment_error_vault_owner_required
    "request_not_approved" -> R.string.peppy_enrollment_error_request_not_approved
    "username_taken" -> R.string.peppy_enrollment_error_username_taken
    "operation_conflict" -> R.string.peppy_enrollment_error_operation_conflict
    "invite_invalid" -> R.string.peppy_enrollment_error_invite_invalid
    "request_not_found" -> R.string.peppy_enrollment_error_request_not_found
    "vault_not_found" -> R.string.peppy_enrollment_error_vault_not_found
    "invalid_username" -> R.string.peppy_enrollment_error_invalid_username
    "invalid_display_name" -> R.string.peppy_enrollment_error_invalid_display_name
    "vault_request_limited" -> R.string.peppy_enrollment_error_vault_request_limited
    "create_vault_limited" -> R.string.peppy_enrollment_error_create_vault_limited
    "manual_owner_limited" -> R.string.peppy_enrollment_error_manual_owner_limited
    else -> null
}

internal fun unlockMessage(result: UnlockResult): String = when (result) {
    UnlockResult.UNLOCKED -> "Vault keys unlocked. Background sync is enabled."
    UnlockResult.NOT_ENROLLED -> "Import a device credential first."
    UnlockResult.DATABASE_UNAVAILABLE -> "The local encrypted database could not be opened with this phone's protected key."
    UnlockResult.WRONG_PASSPHRASE -> "That passphrase does not match this vault."
    UnlockResult.SUPERSEDED -> "The credential changed while unlocking. Enter the passphrase again."
    UnlockResult.FAILED -> "Unlock failed. Try again."
}

@Composable
internal fun syncMessage(status: GatewayStatus): String = when {
    status.syncPhase == SyncPhase.RECOVERY_REQUIRED -> when (status.recoveryReason) {
        RecoveryReason.PRODUCER_CONFLICT -> "Stopped: the server reports this device's updates conflict with an earlier install. Pair this phone again as a new device."
        RecoveryReason.DATABASE_MISSING -> "Stopped: this phone's encrypted message database is missing, so it cannot safely continue. Nothing will be sent. Pair this phone again as a new device."
        else -> "Stopped: this credential was already used by an earlier install. Pair this phone again as a new device."
    }
    status.outboxRejection != null -> "Stopped: the server permanently rejected a queued update (${status.outboxRejection}). Sending and uploads are paused; pair this phone again."
    status.softRejection != null -> "Uploads paused after a server error (${status.softRejection.code}); retrying automatically" +
        (System.currentTimeMillis() - status.softRejection.firstSeenMs).let { ageMs ->
            if (ageMs < 3_600_000L) "." else " (since ~${(ageMs + 1_800_000L) / 3_600_000L} h)."
        }
    !status.sharedKeysReady -> stringResource(R.string.peppy_not_unlocked)
    status.syncPhase == SyncPhase.LIVE -> "Live"
    else -> "Importing vault history (no texts are sent until this finishes)"
}

@OptIn(ExperimentalComposeUiApi::class, ExperimentalMaterial3Api::class)
@Composable
private fun CompanionScreen(activity: ComponentActivity) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var status by remember { mutableStateOf<GatewayStatus?>(null) }
    var refresh by remember { mutableIntStateOf(0) }
    var permissionRefresh by remember { mutableIntStateOf(0) }
    var unlockBusy by remember { mutableStateOf(false) }
    var unlockResult by remember { mutableStateOf<UnlockResult?>(null) }
    var passphrase by remember { mutableStateOf("") }
    var destination by rememberSaveable { mutableStateOf("sms") }
    var settingsOpen by remember { mutableStateOf(false) }
    var appFiltersOpen by remember { mutableStateOf(false) }
    var smsDetailsOpen by remember { mutableStateOf(false) }
    var accountDetailsOpen by remember { mutableStateOf(false) }
    var welcomeDestination by rememberSaveable { mutableStateOf(WelcomeDestination.HOSTED) }
    var ownerPhoneHost by remember { mutableStateOf<OwnerPhoneApprovalHost?>(null) }
    // Plain remember: the owner flow is transient, so it must not outlive its live host (no saved state).
    var addPhoneOpen by remember { mutableStateOf(false) }
    var ownerPhoneAvailable by remember { mutableStateOf(false) }
    val ownerDeviceAuth = rememberDeviceAuthenticator(
        title = stringResource(R.string.peppy_owner_phone_secret_reveal_cta),
        description = stringResource(R.string.peppy_owner_phone_secret_saved_hint),
    )
    val ownerPhoneActions = remember(ownerPhoneHost) { ownerPhoneHost?.actions() }
    val lifecycleOwner = LocalLifecycleOwner.current
    val leavingOwnerPhoneHost = ownerPhoneHost
    // Leaving the flow (done, cancel, back, or recreation) closes the client and cancels any waiting invitation.
    DisposableEffect(leavingOwnerPhoneHost) {
        onDispose {
            leavingOwnerPhoneHost?.let { host ->
                CoroutineScope(Dispatchers.IO).launch {
                    host.cancel()
                    host.close()
                }
            }
        }
    }

    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) permissionRefresh++
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(refresh, permissionRefresh) {
        val current = withContext(Dispatchers.IO) { NativeGateway.status(context) }
        if (current.sharedKeysReady) GatewayWork.ensurePeriodic(context)
        if (current.sharedKeysReady) RemoteMessagingService.resumeOrEnqueue(context)
        MmsCaptureWork.onPermissionOrForeground(context)
        status = current
        ownerPhoneAvailable = current.sharedKeysReady &&
            withContext(Dispatchers.IO) {
                GatewayAccountHost.load(context)?.self?.let { it.role == "owner" && !it.revoked } == true
            }
    }

    LaunchedEffect(status?.enrolled) {
        if (status?.enrolled != true) appFiltersOpen = false
    }

    if (addPhoneOpen && ownerPhoneActions != null) {
        // System back behaves like the screen's Cancel: close only once the invitation is cancelled.
        BackHandler {
            scope.launch {
                if (ownerPhoneActions.cancel() is OwnerPhoneApprovalHost.Result.Cancelled) {
                    ownerPhoneHost = null
                    addPhoneOpen = false
                }
            }
        }
        OwnerPhoneApprovalScreen(
            host = ownerPhoneActions,
            onDone = { ownerPhoneHost = null; addPhoneOpen = false },
            revealSecret = { revealOwnerVaultSecret(context, ownerDeviceAuth) },
        )
        return
    }


    if (welcomeDestination == WelcomeDestination.HOSTED_PAIRING && status?.enrolled == false) {
        EnrollmentScreen(
            initialOrigin = HostedAccountClient.ORIGIN,
            onDismiss = { welcomeDestination = WelcomeDestination.HOSTED_ENROLLMENT },
            onEnrolled = {
                welcomeDestination = WelcomeDestination.HOSTED
                refresh++
            },
        )
        return
    }

    if (status == null) {
        // Edge-to-edge is enabled, so the startup spinner centers within the safe drawing area.
        Box(
            Modifier.fillMaxSize().safeDrawingPadding().testTag("startup-loading-container"),
            contentAlignment = Alignment.Center,
        ) {
            CircularProgressIndicator(Modifier.testTag("startup-loading-indicator"))
        }
        return
    }
    ObserveServerReachability(status?.enrolled == true, refresh)
    if (welcomeDestination == WelcomeDestination.HOSTED_ENROLLMENT && status?.enrolled == false) {
        HostedEnrollmentScreen(
            onDismiss = { welcomeDestination = WelcomeDestination.HOSTED },
            onEnrolled = { welcomeDestination = WelcomeDestination.HOSTED; refresh++ },
            onEnrollExisting = { welcomeDestination = WelcomeDestination.HOSTED_PAIRING },
        )
        return
    }
    if (welcomeDestination == WelcomeDestination.SELF_HOSTED_ENROLLMENT && status?.enrolled == false) {
        EnrollmentScreen(
            onDismiss = { welcomeDestination = WelcomeDestination.SELF_HOSTED },
            onEnrolled = { welcomeDestination = WelcomeDestination.HOSTED; refresh++ },
        )
        return
    }

    if (appFiltersOpen && !settingsOpen && status?.enrolled == true) {
        Column(Modifier.fillMaxSize()) {
            if (status?.sharedKeysReady == true) ServerOfflineBanner()
            NotificationAppFiltersScreen(onBack = {
                appFiltersOpen = false
                refresh++
            })
        }
        return
    }

    BackHandler(enabled = settingsOpen) { settingsOpen = false }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(if (settingsOpen) R.string.peppy_settings else R.string.peppy_app_name)) },
                navigationIcon = {
                    if (settingsOpen) IconButton(onClick = { settingsOpen = false }, Modifier.testTag("settings-back-button")) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.peppy_back))
                    }
                },
            )
        },
        bottomBar = {
            if (status?.enrolled == true && status?.sharedKeysReady == true && !settingsOpen && !smsDetailsOpen && !accountDetailsOpen) NavigationBar {
                listOf("mirroring" to R.string.peppy_mirroring, "sms" to R.string.peppy_sms, "contacts" to R.string.peppy_permissions_contacts, "account" to R.string.peppy_account).forEach { (route, labelRes) ->
                    val label = stringResource(labelRes)
                    val icon = when (route) { "mirroring" -> Icons.Default.Notifications; "sms" -> Icons.Default.Sms; "contacts" -> Icons.Default.Contacts; else -> Icons.Default.Person }
                    NavigationBarItem(selected = destination == route, onClick = { destination = route }, icon = { Icon(icon, contentDescription = label) }, label = { Text(label) }, modifier = Modifier.testTag("tab-$route"))
                }
            }
        },
    ) { innerPadding ->
        Column(Modifier.fillMaxSize().padding(innerPadding)) {
            if (status?.enrolled == true && status?.sharedKeysReady == true) ServerOfflineBanner()
            Column(
                Modifier.fillMaxSize().verticalScroll(rememberScrollState()).then(
                    if (status?.enrolled == true && status?.sharedKeysReady == true) Modifier else Modifier.padding(24.dp),
                )
                    .semantics { testTagsAsResourceId = true }.testTag("companion-screen"),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
        if (status?.enrolled != true && !settingsOpen) {
            when (welcomeDestination) {
                WelcomeDestination.HOSTED -> WelcomeScreen(
                    onGetStarted = { welcomeDestination = WelcomeDestination.HOSTED_ENROLLMENT },
                    onSelectSelfHosted = { welcomeDestination = WelcomeDestination.SELF_HOSTED },
                )
                WelcomeDestination.SELF_HOSTED -> SelfHostedSetupScreen(
                    onBack = { welcomeDestination = WelcomeDestination.HOSTED },
                    onEnroll = { welcomeDestination = WelcomeDestination.SELF_HOSTED_ENROLLMENT },
                    onOpenGuide = {
                        context.startActivity(
                            android.content.Intent(
                                android.content.Intent.ACTION_VIEW,
                                Uri.parse("https://github.com/mattv8/peppy#readme"),
                            ),
                        )
                    },
                )
                else -> Unit
            }
        }

        if (status?.enrolled == true && !status!!.sharedKeysReady && !settingsOpen) Section("lock-screen", stringResource(R.string.peppy_locked), showDivider = false) {
            Text(
                "Enter the existing encryption passphrase you already use on your other devices. This phone does not create a new passphrase.",
                style = MaterialTheme.typography.bodyMedium,
            )
            OutlinedTextField(
                value = passphrase,
                onValueChange = { passphrase = it },
                label = { Text(stringResource(R.string.peppy_passphrase_field)) },
                singleLine = true,
                enabled = !unlockBusy && status?.enrolled == true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                modifier = Modifier.fillMaxWidth().testTag("passphrase-field"),
            )
            BusyButton(
                "unlock-button", "Unlock", unlockBusy,
                enabled = !unlockBusy && status?.enrolled == true && passphrase.isNotEmpty(),
            ) {
                val attempt = passphrase
                passphrase = ""
                unlockBusy = true
                scope.launch {
                    val result = withContext(Dispatchers.IO) { NativeGateway.unlock(context, attempt) }
                    if (result == UnlockResult.UNLOCKED) { NotificationMirrorService.reconcile(); GatewayWork.enqueue(context) }
                    unlockResult = result
                    unlockBusy = false
                    refresh++
                }
            }
            unlockResult?.let { Text(unlockMessage(it), Modifier.testTag("unlock-error"), style = MaterialTheme.typography.bodySmall) }
            TextButton(onClick = { settingsOpen = true }, modifier = Modifier.testTag("locked-settings-button")) { Text(stringResource(R.string.peppy_settings)) }
        }

        if (!settingsOpen && status?.enrolled == true && status?.sharedKeysReady == true && destination == "sms") SmsTab(
            context = context,
            refresh = refresh + permissionRefresh,
            databaseOpen = status?.databaseOpen == true,
            onChanged = { refresh++ },
            gatewayStatus = status,
            onDetailsChanged = { smsDetailsOpen = it },
        )
        if (!settingsOpen && status?.enrolled == true && status?.sharedKeysReady == true && destination == "mirroring") NotificationMirroringSettings(refresh = refresh, onChanged = { refresh++ }, onOpenAppFilters = { appFiltersOpen = true })
        if (!settingsOpen && status?.enrolled == true && status?.sharedKeysReady == true && destination == "contacts") {
            ContactSyncSettings(
                context = context,
                refresh = refresh,
                onChanged = { refresh++ },
            )
        }
        if (!settingsOpen && status?.enrolled == true && status?.sharedKeysReady == true && destination == "account") GatewayAccountSettings(onChanged = { refresh++ }, onOpenSettings = { settingsOpen = true }, onAddPhone = {
            scope.launch { ownerPhoneHost = withContext(Dispatchers.IO) { createOwnerPhoneApprovalHost(context) }; addPhoneOpen = ownerPhoneHost != null }
        }, canAddPhone = ownerPhoneAvailable, onDetailChanged = { accountDetailsOpen = it })
        if (settingsOpen) {
            ScreenBody("settings-screen") {
                Section("about-section", "About") {
                    Text(stringResource(R.string.peppy_encrypted))
                    StatusRow("settings-server", "Server", status?.origin ?: "Not enrolled")
                    TextButton(
                        onClick = {
                            try {
                                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://peppy.pro/privacy")))
                            } catch (_: ActivityNotFoundException) {
                                Toast.makeText(context, "No app is available to open the Privacy Policy.", Toast.LENGTH_SHORT).show()
                            }
                        },
                        modifier = Modifier.testTag("settings-privacy-policy"),
                    ) { Text(stringResource(R.string.peppy_privacy_policy)) }
                    Text(stringResource(R.string.peppy_privacy_policy_scope), style = MaterialTheme.typography.bodySmall)
                }
                DiagnosticsSettings()
                if (status?.sharedKeysReady == true) GatewayAccountSettings(onChanged = { refresh++ }, settingsMode = true)
            }
        }
            }
        }
    }
}

@Composable
internal fun Section(
    tag: String,
    title: String,
    showDivider: Boolean = true,
    helpBody: String? = null,
    content: @Composable () -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag(tag), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (showDivider) HorizontalDivider()
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
            helpBody?.let { InfoButton(title, title, it, "info-$tag") }
        }
        content()
    }
}

@Composable
internal fun BusyButton(tag: String, label: String, busy: Boolean, enabled: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Button(onClick = onClick, enabled = enabled, modifier = modifier.testTag(tag)) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (busy) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            Text(label)
        }
    }
}

@Composable
internal fun StatusRow(tag: String, label: String, value: String) {
    Row(Modifier.fillMaxWidth().testTag(tag), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, style = MaterialTheme.typography.bodyMedium)
        Text(value, style = MaterialTheme.typography.bodyMedium)
    }
}
