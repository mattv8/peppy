package dev.peppy.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import android.icu.text.MeasureFormat
import android.icu.util.Measure
import android.icu.util.MeasureUnit
import java.util.Locale

internal interface OwnerPhoneApprovalActions {
    suspend fun start(method: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result
    suspend fun poll(): OwnerPhoneApprovalHost.Result
    suspend fun rotateIfNeeded(requested: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result
    suspend fun restart(method: OwnerPhoneMethod): OwnerPhoneApprovalHost.Result
    suspend fun approve(prompt: SecretPrompt): OwnerPhoneApprovalHost.Result
    suspend fun cancel(): OwnerPhoneApprovalHost.Result
}

internal fun OwnerPhoneApprovalHost.actions(): OwnerPhoneApprovalActions = object : OwnerPhoneApprovalActions {
    override suspend fun start(method: OwnerPhoneMethod) = this@actions.start(method)
    override suspend fun poll() = this@actions.poll()
    override suspend fun rotateIfNeeded(requested: OwnerPhoneMethod) = this@actions.rotateIfNeeded(requested)
    override suspend fun restart(method: OwnerPhoneMethod) = this@actions.restart(method)
    override suspend fun approve(prompt: SecretPrompt) = this@actions.approve(prompt)
    override suspend fun cancel() = this@actions.cancel()
}

/** Bridges the host's suspending secret request to the approval-fallback dialog. Never saved. */
private class ApprovalPromptBridge : SecretPrompt {
    val request = mutableStateOf<CompletableDeferred<CharArray?>?>(null)

    override suspend fun ask(): CharArray? {
        val answer = CompletableDeferred<CharArray?>()
        withContext(Dispatchers.Main.immediate) { request.value = answer }
        return try {
            answer.await()
        } finally {
            withContext(Dispatchers.Main.immediate) { request.value = null }
        }
    }
}

@Composable
internal fun OwnerPhoneApprovalScreen(
    host: OwnerPhoneApprovalActions,
    onDone: () -> Unit,
    revealSecret: suspend () -> RevealOutcome,
    secureWindow: SecureWindow = rememberActivitySecureWindow(),
) {
    val scope = rememberCoroutineScope()
    val lifecycleOwner = LocalLifecycleOwner.current
    val prompt = remember { ApprovalPromptBridge() }
    // The last good view survives transient failures; an error never replaces the invitation on screen.
    var view by remember { mutableStateOf<OwnerPhoneApprovalHost.Result.View?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var method by remember { mutableStateOf(OwnerPhoneMethod.TRANSFER) }
    var codesMatch by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    // Plain remember only: a revealed secret is never saved across recreation.
    var revealed by remember { mutableStateOf<String?>(null) }
    var revealFailure by remember { mutableStateOf<String?>(null) }

    fun apply(next: OwnerPhoneApprovalHost.Result) {
        when (next) {
            is OwnerPhoneApprovalHost.Result.View -> {
                view = next
                error = null
                if (next.status.stage != OwnerPhoneStage.VERIFY) codesMatch = false
                if (next.status.stage != OwnerPhoneStage.WAITING) revealed = null
            }
            is OwnerPhoneApprovalHost.Result.Failed -> error = next.code
            OwnerPhoneApprovalHost.Result.Cancelled -> Unit
        }
    }
    fun launch(action: suspend () -> OwnerPhoneApprovalHost.Result) {
        if (busy) return
        busy = true
        scope.launch {
            try { apply(action()) } finally { busy = false }
        }
    }
    fun approve() = launch {
        when (val next = host.approve(prompt)) {
            // Cancelling the fallback dialog leaves the verify stage unchanged.
            OwnerPhoneApprovalHost.Result.Cancelled -> host.poll()
            else -> next
        }
    }
    fun restart() = launch { host.restart(method) }
    fun retry() {
        val current = view?.status?.stage
        when {
            current == OwnerPhoneStage.APPROVED -> launch { host.poll() }
            error == "expired" || current == null || current == OwnerPhoneStage.EXPIRED || current == OwnerPhoneStage.DENIED -> restart()
            current == OwnerPhoneStage.VERIFY -> approve()
            else -> launch { host.poll() }
        }
    }
    fun cancel() {
        if (busy) return
        busy = true
        scope.launch {
            try {
                when (val next = host.cancel()) {
                    OwnerPhoneApprovalHost.Result.Cancelled -> onDone()
                    else -> apply(next)
                }
            } finally { busy = false }
        }
    }
    // Reveal rotates the invitation to the shared-secret method first, so a recipient is asked for the secret.
    fun revealSharedSecret() {
        if (busy) return
        busy = true
        scope.launch {
            try {
                val rotated = host.rotateIfNeeded(OwnerPhoneMethod.SHARED_SECRET)
                apply(rotated)
                if (rotated !is OwnerPhoneApprovalHost.Result.View) return@launch
                method = OwnerPhoneMethod.SHARED_SECRET
                when (val outcome = revealSecret()) {
                    is RevealOutcome.Revealed -> {
                        revealed = String(outcome.secret)
                        outcome.secret.fill('\u0000')
                        revealFailure = null
                    }
                    is RevealOutcome.Failed -> {
                        revealed = null
                        revealFailure = outcome.code
                    }
                    RevealOutcome.Cancelled -> Unit
                }
            } finally { busy = false }
        }
    }
    fun toggleReveal() {
        if (revealed != null) revealed = null else revealSharedSecret()
    }

    val isRevealed = revealed != null
    DisposableEffect(isRevealed) {
        if (isRevealed) secureWindow.setSecure(true)
        onDispose { if (isRevealed) secureWindow.setSecure(false) }
    }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) revealed = null
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(host) {
        apply(host.start(OwnerPhoneMethod.TRANSFER))
        while (true) {
            delay(2_000)
            // Skip while an action is in flight so a stale VERIFY poll cannot overwrite an APPROVED result.
            if (busy) continue
            when (view?.status?.stage) {
                OwnerPhoneStage.WAITING -> apply(host.rotateIfNeeded(method))
                OwnerPhoneStage.VERIFY -> apply(host.poll())
                OwnerPhoneStage.APPROVED -> apply(host.poll())
                // CONNECTED, EXPIRED, and DENIED are terminal for polling.
                else -> Unit
            }
        }
    }

    val pendingPrompt = prompt.request.value
    if (pendingPrompt != null) ApprovalPassphraseDialog(pendingPrompt)

    val stage = view?.status?.stage
    Column(
        Modifier.fillMaxSize().safeDrawingPadding().padding(24.dp).testTag("owner-phone-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        when (stage) {
            OwnerPhoneStage.WAITING -> {
                Text(stringResource(R.string.peppy_owner_phone_waiting_headline))
                Text(stringResource(R.string.peppy_owner_phone_waiting_body))
                QrCode(view?.qr.orEmpty(), Modifier.fillMaxWidth().weight(1f).testTag("owner-phone-qr"))
                val expiresInSeconds = MeasureFormat.getInstance(Locale.getDefault(), MeasureFormat.FormatWidth.WIDE)
                    .format(Measure(view?.status?.expiresInSeconds ?: 0, MeasureUnit.SECOND))
                Text(
                    "${stringResource(R.string.peppy_owner_phone_countdown_label)} $expiresInSeconds",
                    Modifier.testTag("owner-phone-countdown"),
                )
                Text(stringResource(R.string.peppy_owner_phone_waiting_hint), style = MaterialTheme.typography.bodySmall)
                OutlinedButton(
                    onClick = { toggleReveal() },
                    enabled = !busy,
                    modifier = Modifier.testTag("owner-phone-shared-secret"),
                ) {
                    Text(
                        stringResource(
                            if (isRevealed) R.string.peppy_owner_phone_secret_hide_cta else R.string.peppy_owner_phone_shared_secret_cta,
                        ),
                    )
                }
                revealed?.let { secret ->
                    Text(secret, Modifier.testTag("owner-phone-revealed-secret"), style = MaterialTheme.typography.titleMedium)
                }
                cancelButton(::cancel)
            }
            OwnerPhoneStage.VERIFY -> {
                val sas = view?.status?.sas.orEmpty()
                val sasLabel = stringResource(R.string.peppy_production_pairing_sas_label)
                val spokenSas = sas.toCharArray().joinToString(" ")
                Text(stringResource(R.string.peppy_owner_phone_verify_headline))
                Text(stringResource(R.string.peppy_owner_phone_verify_body))
                Text(
                    sas,
                    Modifier.testTag("owner-phone-sas").semantics { contentDescription = "$sasLabel $spokenSas" },
                    style = MaterialTheme.typography.headlineMedium,
                )
                Row(
                    Modifier.fillMaxWidth().testTag("owner-phone-codes-match").toggleable(codesMatch, role = Role.Checkbox) { codesMatch = it },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Checkbox(codesMatch, null)
                    Text(stringResource(R.string.peppy_owner_phone_codes_match), Modifier.padding(start = 8.dp))
                }
                Button(onClick = { approve() }, enabled = codesMatch && !busy, modifier = Modifier.testTag("owner-phone-approve")) {
                    if (busy) CircularProgressIndicator(Modifier.size(16.dp)) else Text(stringResource(R.string.peppy_owner_phone_approve_cta))
                }
                cancelButton(::cancel)
            }
            OwnerPhoneStage.CONNECTED -> {
                Text(stringResource(R.string.peppy_owner_phone_connected_headline), Modifier.testTag("owner-phone-connected").semantics { liveRegion = LiveRegionMode.Assertive })
                Text(stringResource(R.string.peppy_owner_phone_connected_body))
                Button(onClick = onDone) { Text(stringResource(R.string.peppy_owner_phone_done_cta)) }
            }
            OwnerPhoneStage.EXPIRED -> {
                Text(stringResource(R.string.peppy_owner_phone_expired_headline))
                Text(stringResource(R.string.peppy_owner_phone_expired_body), Modifier.semantics { liveRegion = LiveRegionMode.Assertive })
                Button(onClick = { restart() }, modifier = Modifier.testTag("owner-phone-restart")) { Text(stringResource(R.string.peppy_owner_phone_restart_cta)) }
                cancelButton(::cancel)
            }
            OwnerPhoneStage.APPROVED -> {
                Text(stringResource(R.string.peppy_owner_phone_approved_body), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                CircularProgressIndicator()
                cancelButton(::cancel)
            }
            OwnerPhoneStage.DENIED -> {
                Text(stringResource(R.string.peppy_owner_phone_error_denied), Modifier.testTag("owner-phone-error").semantics { liveRegion = LiveRegionMode.Assertive }, color = MaterialTheme.colorScheme.error)
                Button(onClick = { restart() }, modifier = Modifier.testTag("owner-phone-restart")) { Text(stringResource(R.string.peppy_owner_phone_restart_cta)) }
                cancelButton(::cancel)
            }
            null -> {
                Text(stringResource(R.string.peppy_owner_phone_waiting_headline))
                if (busy) CircularProgressIndicator()
            }
        }
        error?.let { code ->
            Text(ownerPhoneErrorMessage(code), Modifier.testTag("owner-phone-error").semantics { liveRegion = LiveRegionMode.Assertive }, color = MaterialTheme.colorScheme.error)
            Button(onClick = ::retry, enabled = !busy) { Text(stringResource(R.string.peppy_try_again)) }
            cancelButton(::cancel)
        }
        revealFailure?.let {
            Text(ownerPhoneErrorMessage(it), Modifier.testTag("owner-phone-reveal-error").semantics { liveRegion = LiveRegionMode.Assertive }, color = MaterialTheme.colorScheme.error)
        }
    }
}

@Composable
private fun ApprovalPassphraseDialog(request: CompletableDeferred<CharArray?>) {
    var secret by remember(request) { mutableStateOf("") }
    val focusRequester = remember(request) { FocusRequester() }

    LaunchedEffect(request) { focusRequester.requestFocus() }

    fun dismiss() {
        secret = ""
        request.complete(null)
    }
    fun confirm() {
        val chars = secret.toCharArray()
        secret = ""
        if (!request.complete(chars)) chars.fill('\u0000')
    }

    AlertDialog(
        onDismissRequest = ::dismiss,
        title = { Text(stringResource(R.string.peppy_owner_phone_prompt_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.peppy_owner_phone_prompt_body), style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(
                    value = secret,
                    onValueChange = { secret = it },
                    label = { Text(stringResource(R.string.peppy_passphrase_field)) },
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().focusRequester(focusRequester).testTag("owner-phone-prompt-field"),
                )
            }
        },
        confirmButton = {
            Button(onClick = ::confirm, enabled = secret.isNotEmpty(), modifier = Modifier.testTag("owner-phone-prompt-confirm")) {
                Text(stringResource(R.string.peppy_continue))
            }
        },
        dismissButton = {
            OutlinedButton(onClick = ::dismiss, modifier = Modifier.testTag("owner-phone-prompt-cancel")) {
                Text(stringResource(R.string.peppy_cancel))
            }
        },
        modifier = Modifier.testTag("owner-phone-approval-dialog"),
    )
}

/** Renders the QR at a density-scaled size (280 dp) so it stays crisp on every screen density. */
@Composable
private fun QrCode(value: String, modifier: Modifier) {
    val accessibilityLabel = stringResource(R.string.peppy_owner_phone_qr_accessibility)
    val sizePx = with(LocalDensity.current) { QR_SIZE_DP.dp.roundToPx() }
    val image = remember(value, sizePx) {
        val matrix = QRCodeWriter().encode(value.ifEmpty { " " }, BarcodeFormat.QR_CODE, sizePx, sizePx)
        Bitmap.createBitmap(sizePx, sizePx, Bitmap.Config.ARGB_8888).also { bitmap ->
            for (y in 0 until sizePx) for (x in 0 until sizePx) bitmap.setPixel(x, y, if (matrix[x, y]) android.graphics.Color.BLACK else android.graphics.Color.WHITE)
        }.asImageBitmap()
    }
    Box(modifier.semantics { contentDescription = accessibilityLabel }, contentAlignment = Alignment.Center) {
        Image(image, null, Modifier.fillMaxSize())
    }
}

private const val QR_SIZE_DP = 280

@Composable
private fun cancelButton(onClick: () -> Unit) {
    OutlinedButton(onClick = onClick, modifier = Modifier.testTag("owner-phone-cancel")) { Text(stringResource(R.string.peppy_cancel)) }
}

@Composable
private fun ownerPhoneErrorMessage(code: String): String = stringResource(when (code) {
    "wrong-passphrase" -> R.string.peppy_owner_phone_error_wrong_passphrase
    "vault-secret-mismatch" -> R.string.peppy_owner_phone_error_secret_mismatch
    "vault-secret-unavailable" -> R.string.peppy_owner_phone_error_secret_unavailable
    "secure-store-unavailable" -> R.string.peppy_production_secure_storage_failed
    "lock-required" -> R.string.peppy_owner_phone_error_lock_required
    "account-required" -> R.string.peppy_owner_phone_error_account_required
    "enrollment-state" -> R.string.peppy_owner_phone_error_state
    "server-rejected" -> R.string.peppy_owner_phone_error_server_rejected
    "expired" -> R.string.peppy_owner_phone_expired_body
    else -> R.string.peppy_production_network_failed
})
