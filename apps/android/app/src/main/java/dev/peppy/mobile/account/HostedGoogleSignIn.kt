package dev.peppy.mobile.account

import androidx.activity.ComponentActivity
import androidx.credentials.CredentialManager
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.exceptions.GetCredentialCancellationException
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import dev.peppy.mobile.BuildConfig

internal class HostedGoogleSignInFailure(val reason: Reason) : Exception(reason.name) {
    enum class Reason { CONFIGURATION, PROVIDER_UNAVAILABLE, UNEXPECTED_CREDENTIAL }
}

/** Returns null only when the user dismisses the system credential UI. */
internal suspend fun <T> silentCredentialCancellation(request: suspend () -> T): T? = try {
    request()
} catch (_: GetCredentialCancellationException) {
    null
}

/** Account-session sign-in only; it does not touch enrollment drafts, vault credentials, or keys. */
internal suspend fun signInHostedAccount(activity: ComponentActivity, client: HostedAccountClient): String? {
    if (BuildConfig.PEPPY_GOOGLE_SERVER_CLIENT_ID.isBlank()) {
        throw HostedGoogleSignInFailure(HostedGoogleSignInFailure.Reason.CONFIGURATION)
    }
    if ("google" !in client.availableProviders()) {
        throw HostedGoogleSignInFailure(HostedGoogleSignInFailure.Reason.PROVIDER_UNAVAILABLE)
    }
    client.beginGoogleSignIn().use { attempt ->
        val option = GetSignInWithGoogleOption.Builder(BuildConfig.PEPPY_GOOGLE_SERVER_CLIENT_ID)
            .setNonce(attempt.nonce())
            .build()
        val credential = silentCredentialCancellation {
            CredentialManager.create(activity).getCredential(
                activity,
                GetCredentialRequest.Builder().addCredentialOption(option).build(),
            ).credential
        } ?: return null
        if (credential !is CustomCredential || credential.type != GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL) {
            throw HostedGoogleSignInFailure(HostedGoogleSignInFailure.Reason.UNEXPECTED_CREDENTIAL)
        }
        return client.finishGoogleSignIn(attempt.attemptId(), GoogleIdTokenCredential.createFrom(credential.data).idToken).accountId
    }
}
