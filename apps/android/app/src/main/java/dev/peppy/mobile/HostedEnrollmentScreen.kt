package dev.peppy.mobile

import android.content.Intent
import android.net.Uri
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.credentials.CredentialManager
import androidx.credentials.CustomCredential
import androidx.credentials.ClearCredentialStateRequest
import androidx.credentials.GetCredentialRequest
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.NativeHostedProvisioningView
import uniffi.peppy_mobile_bindings.generateHostedPassphrase

internal sealed interface HostedPhase {
    data object Start : HostedPhase
    data object SigningIn : HostedPhase
    data object Checking : HostedPhase
    data object Billing : HostedPhase
    data object Passphrase : HostedPhase
    data object Confirm : HostedPhase
    data object Provisioning : HostedPhase
    data object ExistingVault : HostedPhase
    data class Error(val message: String) : HostedPhase
}

/** Retained across rotation in memory only; never uses SavedStateHandle or saved-state serialization. */
internal class HostedEnrollmentDraft : ViewModel() {
    var phase by mutableStateOf<HostedPhase>(HostedPhase.Start)
    var passphrase by mutableStateOf<String?>(null)
    var acknowledged by mutableStateOf(false)
    var accountId: String? = null
    fun clear() { phase = HostedPhase.Start; passphrase = null; acknowledged = false; accountId = null }
    override fun onCleared() { clear() }
}

@Composable
internal fun HostedEnrollmentScreen(
    onDismiss: () -> Unit,
    onEnrolled: () -> Unit,
    onPairExisting: () -> Unit,
) {
    val context = LocalContext.current
    val appContext = context.applicationContext
    val activity = context as ComponentActivity
    val draft = remember(activity) { ViewModelProvider(activity)[HostedEnrollmentDraft::class.java] }
    val scope = rememberCoroutineScope()
    val client = remember(appContext) { HostedAccountClient(AndroidHostedSecureStore(appContext)) }
    var phase by draft::phase
    var displayedPassphrase by draft::passphrase
    var confirmation by remember { mutableStateOf("") }
    var acknowledged by draft::acknowledged
    var confirmationError by remember { mutableStateOf(false) }
    var work by remember { mutableStateOf<Job?>(null) }
    var generation by remember { mutableStateOf(0L) }

    fun clearSecrets() {
        draft.clear()
        confirmation = ""
        confirmationError = false
    }
    fun cancelWork(clearDraft: Boolean = true) {
        generation++
        work?.cancel()
        work = null
        client.cancelPendingWork()
        if (clearDraft) clearSecrets() else confirmation = ""
    }
    fun dismiss() {
        cancelWork()
        onDismiss()
    }
    fun fail(message: String) { phase = HostedPhase.Error(message) }
    fun startWork(block: suspend (Long) -> Unit) {
        work?.cancel()
        client.cancelPendingWork()
        val stamp = ++generation
        work = scope.launch {
            try {
                block(stamp)
            } catch (_: CancellationException) {
                // Cancellation and stale results never change the displayed state.
            }
        }
    }
    fun isCurrent(stamp: Long) = stamp == generation
    fun reconcile() = startWork { stamp ->
        phase = HostedPhase.Checking
        try {
            val pending = client.pendingProvisioning()?.use { it.view() }
            val account = client.account()
            if (!isCurrent(stamp)) return@startWork
            if (draft.accountId != null && draft.accountId != account.accountId) clearSecrets()
            draft.accountId = account.accountId
            when {
                pending != null && account.vaultId != null && account.vaultId != pending.vaultId -> phase = HostedPhase.ExistingVault
                pending != null -> phase = HostedPhase.Confirm
                account.vaultId != null -> phase = HostedPhase.ExistingVault
                account.access == "read_write" -> {
                    if (displayedPassphrase == null) displayedPassphrase = generateHostedPassphrase()
                    phase = if (acknowledged) HostedPhase.Confirm else HostedPhase.Passphrase
                }
                else -> phase = HostedPhase.Billing
            }
        } catch (failure: HostedFailure) {
            if (!isCurrent(stamp)) return@startWork
            when (failure.kind) {
                HostedFailure.Kind.SESSION_EXPIRED -> phase = HostedPhase.Start
                HostedFailure.Kind.WRONG_ACCOUNT -> fail(context.getString(R.string.peppy_production_account_mismatch))
                else -> fail(context.getString(R.string.peppy_production_network_failed))
            }
        } catch (_: Exception) {
            if (isCurrent(stamp)) fail(context.getString(R.string.peppy_production_network_failed))
        }
    }

    LaunchedEffect(client) { reconcile() }
    DisposableEffect(client) {
        val lifecycle = activity.lifecycle
        val wasSecure = activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE != 0
        activity.window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_STOP -> {
                    if (phase == HostedPhase.Passphrase || phase == HostedPhase.Confirm) {
                        // A password-manager hop must not replace the phrase the user is saving.
                        // Keep it only in this volatile presentation; FLAG_SECURE conceals snapshots.
                        confirmation = ""
                    }
                    else if (phase == HostedPhase.SigningIn) {
                        clearSecrets()
                        // The provider's native activity is part of this sign-in operation.
                        phase = HostedPhase.SigningIn
                    }
                    else {
                        val resume = phase == HostedPhase.Provisioning
                        cancelWork(clearDraft = false)
                        if (resume) phase = HostedPhase.Checking
                    }
                }
                Lifecycle.Event.ON_START -> if (phase == HostedPhase.Billing || phase == HostedPhase.Checking) reconcile()
                else -> Unit
            }
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            if (!wasSecure) activity.window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
            cancelWork(clearDraft = !activity.isChangingConfigurations)
        }
    }

    Column(
        Modifier.fillMaxSize().padding(24.dp).testTag("hosted-enrollment-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        when (val current = phase) {
            HostedPhase.Start -> {
                Text(stringResource(R.string.peppy_production_hosted_cta))
                Button(onClick = {
                    if (BuildConfig.PEPPY_GOOGLE_SERVER_CLIENT_ID.isBlank()) {
                        fail(context.getString(R.string.peppy_production_provider_unconfigured))
                        return@Button
                    }
                    startWork { stamp ->
                        phase = HostedPhase.SigningIn
                        try {
                            val providers = client.availableProviders()
                            if ("google" !in providers) throw HostedFailure(HostedFailure.Kind.UNAVAILABLE)
                            client.beginGoogleSignIn().use { login ->
                                if (!isCurrent(stamp)) return@startWork
                                val option = GetSignInWithGoogleOption.Builder(BuildConfig.PEPPY_GOOGLE_SERVER_CLIENT_ID)
                                    .setNonce(login.nonce())
                                    .build()
                                val credential = CredentialManager.create(context)
                                    .getCredential(
                                        context as ComponentActivity,
                                        GetCredentialRequest.Builder().addCredentialOption(option).build(),
                                    ).credential
                                if (credential !is CustomCredential ||
                                    credential.type != GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
                                ) throw IllegalStateException("Unexpected credential type")
                                val idToken = GoogleIdTokenCredential.createFrom(credential.data).idToken
                                client.finishGoogleSignIn(login.attemptId(), idToken)
                                if (isCurrent(stamp)) reconcile()
                            }
                        } catch (_: GetCredentialCancellationException) {
                            if (isCurrent(stamp)) phase = HostedPhase.Start
                        } catch (_: CancellationException) {
                            throw CancellationException()
                        } catch (_: Exception) {
                            if (isCurrent(stamp)) fail(context.getString(R.string.peppy_production_signin_failed))
                        }
                    }
                }, Modifier.fillMaxWidth().testTag("signin-google-button")) {
                    Text(stringResource(R.string.peppy_hosted_sign_in_google))
                }
                OutlinedButton(onClick = ::dismiss, modifier = Modifier.fillMaxWidth().testTag("hosted-cancel-button")) {
                    Text(stringResource(R.string.peppy_back))
                }
            }
            HostedPhase.SigningIn, HostedPhase.Checking -> CircularProgressIndicator(Modifier.testTag("hosted-signin-inflight"))
            HostedPhase.Billing -> {
                Text(stringResource(R.string.peppy_production_billing_body))
                Button(
                    onClick = { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://peppy.pro/account/subscribe"))) },
                    modifier = Modifier.testTag("hosted-subscribe-button"),
                ) { Text(stringResource(R.string.peppy_production_billing_open)) }
                OutlinedButton(onClick = ::reconcile, modifier = Modifier.testTag("hosted-billing-check-again")) {
                    Text(stringResource(R.string.peppy_production_billing_check))
                }
            }
            HostedPhase.ExistingVault -> {
                Text(stringResource(R.string.peppy_production_existing_vault))
                OutlinedButton(onClick = {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://peppy.pro/account")))
                }, modifier = Modifier.testTag("hosted-existing-manage-account")) {
                    Text(stringResource(R.string.peppy_production_manage_account))
                }
                Button(onClick = onPairExisting, modifier = Modifier.testTag("hosted-pair-existing-button")) {
                    Text(stringResource(R.string.peppy_production_join_cta))
                }
                OutlinedButton(onClick = ::dismiss, modifier = Modifier.testTag("hosted-cancel-button")) { Text(stringResource(R.string.peppy_back)) }
            }
            HostedPhase.Passphrase -> {
                Text(stringResource(R.string.peppy_passphrase_create_headline))
                Text(displayedPassphrase.orEmpty(), Modifier.testTag("passphrase-display"))
                androidx.compose.material3.Checkbox(acknowledged, { acknowledged = it }, Modifier.testTag("passphrase-saved-checkbox"))
                Text(stringResource(R.string.peppy_passphrase_ack_label))
                Button(onClick = { phase = HostedPhase.Confirm }, enabled = acknowledged, modifier = Modifier.testTag("passphrase-continue-button")) {
                    Text(stringResource(R.string.peppy_continue))
                }
            }
            HostedPhase.Confirm -> {
                Text(stringResource(R.string.peppy_passphrase_confirm_headline))
                OutlinedTextField(
                    confirmation,
                    { confirmation = it; confirmationError = false },
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = KeyboardType.Password),
                    modifier = Modifier.fillMaxWidth().testTag("passphrase-confirm-field"),
                )
                if (confirmationError) Text(stringResource(R.string.peppy_passphrase_mismatch), Modifier.testTag("passphrase-confirm-error"))
                Button(onClick = {
                    val entered = confirmation
                    val fresh = displayedPassphrase
                    if (fresh != null && entered != fresh) {
                        confirmationError = true
                        return@Button
                    }
                    startWork { stamp ->
                        phase = HostedPhase.Provisioning
                        try {
                            val view: NativeHostedProvisioningView = client.prepareVault(entered)
                            val credential = client.completeVault(entered)
                            val imported = withContext(Dispatchers.IO) {
                                try { NativeGateway.importCredential(appContext, credential) { isCurrent(stamp) && work?.isActive == true } }
                                finally { credential.fill(0) }
                            }
                            if (imported != ImportResult.IMPORTED && imported != ImportResult.UPDATED) {
                                if (isCurrent(stamp)) fail(context.getString(R.string.peppy_production_provision_retry))
                                return@startWork
                            }
                            client.acknowledgeEnrollment(view.vaultId, view.deviceId)
                            val unlocked = withContext(Dispatchers.IO) { NativeGateway.unlock(appContext, entered) }
                            clearSecrets()
                            if (!isCurrent(stamp)) return@startWork
                            if (unlocked == UnlockResult.UNLOCKED) GatewayWork.enqueue(appContext)
                            onEnrolled()
                        } catch (failure: HostedFailure) {
                            if (!isCurrent(stamp)) return@startWork
                            when (failure.kind) {
                                HostedFailure.Kind.WRONG_ACCOUNT -> fail(context.getString(R.string.peppy_production_account_mismatch))
                                HostedFailure.Kind.WRONG_PASSPHRASE -> fail(context.getString(R.string.peppy_passphrase_mismatch))
                                HostedFailure.Kind.EXISTING_VAULT -> phase = HostedPhase.ExistingVault
                                else -> fail(context.getString(R.string.peppy_production_provision_retry))
                            }
                        } catch (_: Exception) {
                            if (isCurrent(stamp)) fail(context.getString(R.string.peppy_production_provision_retry))
                        }
                    }
                }, Modifier.testTag("passphrase-confirm-button")) { Text(stringResource(R.string.peppy_continue)) }
            }
            HostedPhase.Provisioning -> CircularProgressIndicator(Modifier.testTag("provisioning-screen"))
            is HostedPhase.Error -> {
                Text(current.message, Modifier.testTag("hosted-error"))
                OutlinedButton(onClick = { confirmation = ""; confirmationError = false; reconcile() }, Modifier.testTag("hosted-retry-button")) {
                    Text(stringResource(R.string.peppy_try_again))
                }
            }
        }
        if (phase != HostedPhase.Start && phase != HostedPhase.SigningIn && phase != HostedPhase.Checking && phase != HostedPhase.Provisioning) {
            OutlinedButton(onClick = {
                startWork { stamp ->
                    try {
                        client.signOut()
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) {
                        if (isCurrent(stamp)) fail(context.getString(R.string.peppy_production_secure_storage_failed))
                        return@startWork
                    }
                    try { CredentialManager.create(context).clearCredentialState(ClearCredentialStateRequest()) }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) { /* The next attempt still obtains a fresh, server-bound nonce. */ }
                    if (isCurrent(stamp)) { clearSecrets(); phase = HostedPhase.Start }
                }
            }, modifier = Modifier.testTag("hosted-signout-button")) {
                Text(stringResource(R.string.peppy_settings_server_sign_out))
            }
        }
    }
}
