package dev.peppy.mobile

import android.util.Log

internal enum class HostedAuthStage(val wireValue: String) {
    PROVIDERS("providers"),
    ATTEMPT("attempt"),
    CREDENTIAL_SDK("credential_sdk"),
    TOKEN_PARSE("token_parse"),
    SESSION_EXCHANGE("session_exchange"),
    RECONCILIATION("reconciliation"),
    LIFECYCLE("lifecycle"),
    COMPOSITION("composition"),
    DISPOSAL("disposal"),
}

internal enum class HostedAuthOutcome(val wireValue: String) {
    STARTED("started"),
    SUCCESS("success"),
    FAILURE("failure"),
    CANCELLED("cancelled"),
    STALE("stale"),
    RECEIVED("received"),
    SESSION_EXPIRED("session_expired"),
    RETURNED_START("returned_start"),
    ON_STOP("on_stop"),
    ON_START("on_start"),
    RECREATING("recreating"),
    DISPOSED("disposed"),
}

internal class HostedAuthDiagnostics(
    private val enabled: Boolean,
    private val output: (String) -> Unit = { Log.i(TAG, it) },
) {
    fun record(stage: HostedAuthStage, outcome: HostedAuthOutcome) {
        if (!enabled) return
        try {
            output("stage=${stage.wireValue} outcome=${outcome.wireValue}")
        } catch (_: Exception) {
            // Diagnostics must not affect authentication.
        }
    }

    private companion object {
        const val TAG = "PeppyHostedAuth"
    }
}
