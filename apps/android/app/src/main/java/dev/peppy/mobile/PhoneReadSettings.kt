package dev.peppy.mobile

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal data class PhoneReadPresentation(
    val state: State,
    val toggleEnabled: Boolean,
    val showOpenAccess: Boolean = false,
) {
    enum class State { LOADING, SAVING, ERROR, UNAVAILABLE, SMS_SYNC_REQUIRED, SMS_READ_REQUIRED, ACCESS_DENIED, OFF_READY, WORKING, ACCESS_PAUSED, SMS_SYNC_PAUSED, SMS_READ_PAUSED }
}

internal data class PhoneReadReadback(val enabled: Boolean?, val failed: Boolean)

internal data class PhoneReadAccessResult(val consent: ConsentGatedToggle, val apply: Boolean, val denied: Boolean)

internal fun phoneReadReadback(lastKnown: Boolean?, requested: Boolean?, readBack: Boolean?): PhoneReadReadback =
    if (readBack != null && (requested == null || readBack == requested)) PhoneReadReadback(readBack, false)
    else PhoneReadReadback(lastKnown, true)

internal fun phoneReadAccessResult(consent: ConsentGatedToggle, granted: Boolean): PhoneReadAccessResult {
    val transition = consent.accessResult(granted)
    return PhoneReadAccessResult(transition.state, transition.effect == ConsentEffect.APPLY, !granted && consent.phase == ConsentPhase.AWAITING_ACCESS)
}

internal fun phoneReadOperationIsCurrent(operation: Int, current: Int): Boolean = operation == current

internal fun phoneReadPresentation(
    enabled: Boolean?,
    smsSyncEnabled: Boolean,
    smsReadAccess: Boolean,
    notificationAccess: Boolean,
    supported: Boolean,
    loading: Boolean = false,
    saving: Boolean = false,
    failed: Boolean = false,
    accessDenied: Boolean = false,
): PhoneReadPresentation = when {
    !supported -> PhoneReadPresentation(PhoneReadPresentation.State.UNAVAILABLE, false)
    enabled == null && loading -> PhoneReadPresentation(PhoneReadPresentation.State.LOADING, false)
    enabled == null || failed -> PhoneReadPresentation(PhoneReadPresentation.State.ERROR, false, enabled == true && !notificationAccess)
    saving -> PhoneReadPresentation(PhoneReadPresentation.State.SAVING, false, enabled && !notificationAccess)
    !enabled && !smsSyncEnabled -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_SYNC_REQUIRED, false)
    !enabled && !smsReadAccess -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_READ_REQUIRED, false)
    !enabled && accessDenied -> PhoneReadPresentation(PhoneReadPresentation.State.ACCESS_DENIED, true, showOpenAccess = true)
    !enabled -> PhoneReadPresentation(PhoneReadPresentation.State.OFF_READY, true)
    !smsSyncEnabled -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_SYNC_PAUSED, true, showOpenAccess = !notificationAccess)
    !smsReadAccess -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_READ_PAUSED, true, showOpenAccess = !notificationAccess)
    !notificationAccess -> PhoneReadPresentation(PhoneReadPresentation.State.ACCESS_PAUSED, true, showOpenAccess = true)
    else -> PhoneReadPresentation(PhoneReadPresentation.State.WORKING, true)
}

@Composable
internal fun PhoneReadSettings(context: Context, refresh: Int, onChanged: () -> Unit) {
    val scope = rememberCoroutineScope()
    val lifecycleOwner = LocalLifecycleOwner.current
    var accessRefresh by remember { mutableIntStateOf(0) }
    var enabled by remember { mutableStateOf<Boolean?>(null) }
    var smsSyncEnabled by remember { mutableStateOf<Boolean?>(null) }
    var notificationAccess by remember { mutableStateOf(false) }
    var consent by remember { mutableStateOf(ConsentGatedToggle()) }
    var loading by remember { mutableStateOf(true) }
    var saving by remember { mutableStateOf(false) }
    var readbackFailed by remember { mutableStateOf(false) }
    var accessDenied by remember { mutableStateOf(false) }
    var operationGeneration by remember { mutableIntStateOf(0) }
    val supported = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
    val smsReadAccess = ContextCompat.checkSelfPermission(context, android.Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED

    fun load() {
        if (saving) return
        val operation = ++operationGeneration
        if (enabled == null) loading = true
        scope.launch {
            val settings = try {
                withContext(Dispatchers.IO) {
                    val host = GatewayPolicyHost(context)
                    host.phoneReadEnabledOrNull() to host.smsCaptureEnabled
                }
            } catch (_: Exception) {
                null
            }
            if (phoneReadOperationIsCurrent(operation, operationGeneration)) {
                val readback = phoneReadReadback(enabled, requested = null, readBack = settings?.first)
                enabled = readback.enabled
                readbackFailed = readback.failed
                settings?.second?.let { smsSyncEnabled = it }
                loading = false
            }
        }
    }

    fun writeAndRead(value: Boolean) {
        val operation = ++operationGeneration
        saving = true
        readbackFailed = false
        scope.launch {
            val stored = try {
                withContext(Dispatchers.IO) {
                    val host = GatewayPolicyHost(context)
                    host.setPhoneReadEnabled(value)
                    host.phoneReadEnabledOrNull()
                }
            } catch (_: Exception) {
                null
            }
            if (phoneReadOperationIsCurrent(operation, operationGeneration)) {
                val readback = phoneReadReadback(enabled, requested = value, readBack = stored)
                enabled = readback.enabled
                readbackFailed = readback.failed
                saving = false
                if (!readback.failed) onChanged()
                consent = consent.cancel()
            }
        }
    }

    fun consumeAccessResult(granted: Boolean) {
        val result = phoneReadAccessResult(consent, granted)
        consent = result.consent
        if (result.apply) writeAndRead(true)
        if (result.denied) accessDenied = true
    }

    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        consumeAccessResult(NotificationMirrorAccess.granted(context))
    }

    LaunchedEffect(refresh) { load() }
    LaunchedEffect(accessRefresh) {
        notificationAccess = NotificationMirrorAccess.granted(context)
        if (notificationAccess) accessDenied = false
    }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) accessRefresh++ }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            consent = consent.cancel()
            lifecycleOwner.lifecycle.removeObserver(observer)
        }
    }

    val presentation = phoneReadPresentation(enabled, smsSyncEnabled == true, smsReadAccess, notificationAccess, supported, loading, saving, readbackFailed, accessDenied)
    val subtitle = when (presentation.state) {
        PhoneReadPresentation.State.LOADING -> null
        PhoneReadPresentation.State.SAVING -> stringResource(R.string.peppy_toggle_android_phone_read_saving)
        PhoneReadPresentation.State.ERROR -> stringResource(R.string.peppy_toggle_android_phone_read_error)
        PhoneReadPresentation.State.UNAVAILABLE -> stringResource(R.string.peppy_toggle_android_phone_read_unavailable)
        PhoneReadPresentation.State.SMS_SYNC_REQUIRED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_required)
        PhoneReadPresentation.State.SMS_READ_REQUIRED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_read_required)
        PhoneReadPresentation.State.ACCESS_DENIED -> stringResource(R.string.peppy_toggle_android_phone_read_access_denied)
        PhoneReadPresentation.State.OFF_READY -> stringResource(R.string.peppy_toggle_android_phone_read_off_hint)
        PhoneReadPresentation.State.WORKING -> stringResource(R.string.peppy_toggle_android_phone_read_on_hint)
        PhoneReadPresentation.State.ACCESS_PAUSED -> stringResource(if (accessDenied) R.string.peppy_toggle_android_phone_read_access_denied else R.string.peppy_toggle_android_phone_read_access_paused)
        PhoneReadPresentation.State.SMS_SYNC_PAUSED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_paused)
        PhoneReadPresentation.State.SMS_READ_PAUSED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_read_paused)
    }

    PhoneReadSettingsContent(
        presentation = presentation,
        checked = enabled == true,
        subtitle = subtitle,
        onCheckedChange = { requested -> if (requested) consent = consent.requestEnable() else writeAndRead(false) },
        onOpenAccess = { accessLauncher.launch(NotificationMirrorAccess.settingsIntent()) },
        onRetry = ::load,
    )

    if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_phone_read_consent_title),
        body = stringResource(R.string.peppy_phone_read_disclosure),
        tag = "phone-read-consent-sheet",
        disclosureTag = "phone-read-disclosure",
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = if (notificationAccess) null else stringResource(R.string.peppy_toggle_android_phone_read_next),
        onDismiss = { consent = consent.cancel() },
        onConfirm = {
            val currentAccess = NotificationMirrorAccess.granted(context)
            notificationAccess = currentAccess
            val transition = consent.agree(currentAccess)
            consent = transition.state
            when (transition.effect) {
                ConsentEffect.APPLY -> writeAndRead(true)
                ConsentEffect.REQUEST_ACCESS -> accessLauncher.launch(NotificationMirrorAccess.settingsIntent())
                ConsentEffect.NONE -> Unit
            }
        },
    )
}

@Composable
internal fun PhoneReadSettingsContent(
    presentation: PhoneReadPresentation,
    checked: Boolean,
    subtitle: String?,
    onCheckedChange: (Boolean) -> Unit,
    onOpenAccess: () -> Unit,
    onRetry: () -> Unit,
) {
    Section(
        tag = "phone-read-section",
        title = stringResource(R.string.peppy_toggle_android_phone_read_section),
        helpBody = stringResource(R.string.peppy_help_android_phone_read),
    ) {
        SettingToggleRow(
            title = stringResource(R.string.peppy_toggle_android_phone_read_label),
            checked = checked,
            enabled = presentation.toggleEnabled,
            onCheckedChange = onCheckedChange,
            tag = "phone-read-row",
            subtitle = subtitle,
            stateDescription = subtitle,
        )
        if (presentation.showOpenAccess) OutlinedButton(
            onClick = onOpenAccess,
            modifier = Modifier.testTag("phone-read-open-access"),
        ) { Text(stringResource(R.string.peppy_toggle_android_phone_read_open_access)) }
        if (presentation.state == PhoneReadPresentation.State.ERROR) OutlinedButton(
            onClick = onRetry,
            modifier = Modifier.testTag("phone-read-retry"),
        ) { Text(stringResource(R.string.peppy_toggle_android_retry)) }
    }
}
