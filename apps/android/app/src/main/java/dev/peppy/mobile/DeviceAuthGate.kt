package dev.peppy.mobile

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import android.view.Window
import android.view.WindowManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlin.coroutines.resume

internal enum class DeviceAuthOutcome { AUTHENTICATED, CANCELLED, FAILED, LOCK_REQUIRED }

internal fun interface DeviceAuthenticator {
    suspend fun authenticate(): DeviceAuthOutcome
}

internal sealed interface RevealOutcome {
    /** Caller owns [secret] and must zero it after use. */
    class Revealed(val secret: CharArray) : RevealOutcome
    data class Failed(val code: String) : RevealOutcome
    data object Cancelled : RevealOutcome
}

/** Seam over the activity window's FLAG_SECURE so tests can observe it without a device. */
internal fun interface SecureWindow {
    fun setSecure(enabled: Boolean)
}

/**
 * Device-owner gate before the stored vault secret is shown. API 30+ uses BiometricPrompt with
 * strong biometrics or device credential; API 26-29 uses the keyguard credential confirmation intent.
 */
@Composable
internal fun rememberDeviceAuthenticator(title: String, description: String): DeviceAuthenticator {
    val context = LocalContext.current
    val legacy = remember { LegacyCredentialRequest() }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        legacy.complete(result.resultCode == Activity.RESULT_OK)
    }
    return remember(context, launcher, legacy, title, description) {
        DeviceAuthenticator {
            authenticate(context, title, description, legacy) { intent -> launcher.launch(intent) }
        }
    }
}

private suspend fun authenticate(
    context: Context,
    title: String,
    description: String,
    legacy: LegacyCredentialRequest,
    launchLegacy: (Intent) -> Unit,
): DeviceAuthOutcome {
    val keyguard = context.getSystemService(KeyguardManager::class.java)
    if (keyguard == null || !keyguard.isDeviceSecure) return DeviceAuthOutcome.LOCK_REQUIRED
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        return authenticateWithBiometricPrompt(context, title, description)
    }
    val intent = keyguard.createConfirmDeviceCredentialIntent(title, description)
        ?: return DeviceAuthOutcome.LOCK_REQUIRED
    return if (legacy.await(launchLegacy, intent)) DeviceAuthOutcome.AUTHENTICATED else DeviceAuthOutcome.CANCELLED
}

@RequiresApi(Build.VERSION_CODES.R)
private suspend fun authenticateWithBiometricPrompt(
    context: Context,
    title: String,
    description: String,
): DeviceAuthOutcome = suspendCancellableCoroutine { continuation ->
    val signal = CancellationSignal()
    val prompt = BiometricPrompt.Builder(context)
        .setTitle(title)
        .setDescription(description)
        .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL)
        .build()
    fun finish(outcome: DeviceAuthOutcome) {
        if (continuation.isActive) continuation.resume(outcome)
    }
    continuation.invokeOnCancellation { signal.cancel() }
    prompt.authenticate(signal, context.mainExecutor, object : BiometricPrompt.AuthenticationCallback() {
        override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = finish(DeviceAuthOutcome.AUTHENTICATED)

        override fun onAuthenticationError(errorCode: Int, errString: CharSequence) = finish(
            when (errorCode) {
                BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED,
                BiometricPrompt.BIOMETRIC_ERROR_CANCELED,
                -> DeviceAuthOutcome.CANCELLED
                else -> DeviceAuthOutcome.FAILED
            },
        )
    })
}

/** Holds the single pending keyguard confirmation result for API 26-29. */
private class LegacyCredentialRequest {
    private var pending: CompletableDeferred<Boolean>? = null

    suspend fun await(launch: (Intent) -> Unit, intent: Intent): Boolean {
        val answer = CompletableDeferred<Boolean>()
        pending = answer
        return try {
            launch(intent)
            answer.await()
        } finally {
            pending = null
        }
    }

    fun complete(authenticated: Boolean) {
        pending?.complete(authenticated)
    }
}

/**
 * Reveals the stored vault secret for the enrolled owner binding after device authentication.
 * Caller must clear the returned [RevealOutcome.Revealed] buffer after display.
 */
internal suspend fun revealOwnerVaultSecret(context: Context, authenticator: DeviceAuthenticator): RevealOutcome =
    when (authenticator.authenticate()) {
        DeviceAuthOutcome.AUTHENTICATED -> readOwnerVaultSecret(context)
        DeviceAuthOutcome.LOCK_REQUIRED -> RevealOutcome.Failed("lock-required")
        DeviceAuthOutcome.CANCELLED, DeviceAuthOutcome.FAILED -> RevealOutcome.Cancelled
    }

private suspend fun readOwnerVaultSecret(context: Context): RevealOutcome = withContext(Dispatchers.IO) {
    val credential = NativeGateway.accountCredential(context)
        ?: return@withContext RevealOutcome.Failed("vault-secret-unavailable")
    try {
        when (val stored = VaultSecretStore(AndroidHostedSecureStore(context)).read(credential.vaultId, credential.deviceId)) {
            StoredSecret.Missing -> RevealOutcome.Failed("vault-secret-unavailable")
            is StoredSecret.Present -> RevealOutcome.Revealed(stored.value)
        }
    } catch (_: SecureStoreUnavailable) {
        RevealOutcome.Failed("secure-store-unavailable")
    }
}

@Composable
internal fun rememberActivitySecureWindow(): SecureWindow {
    val context = LocalContext.current
    return remember(context) { ActivitySecureWindow { context.findActivity()?.window } }
}

/**
 * Sets FLAG_SECURE for a reveal and, on hide, restores the window's state from before the reveal. An unconditional
 * clear would drop a flag the activity set for its own reasons.
 */
private class ActivitySecureWindow(private val windowProvider: () -> Window?) : SecureWindow {
    private var secureBeforeReveal: Boolean? = null

    override fun setSecure(enabled: Boolean) {
        val window = windowProvider() ?: return
        if (enabled) {
            if (secureBeforeReveal == null) {
                secureBeforeReveal = (window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE) != 0
            }
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        } else {
            val wasSecure = secureBeforeReveal ?: return
            secureBeforeReveal = null
            if (!wasSecure) window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }
}

private tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
