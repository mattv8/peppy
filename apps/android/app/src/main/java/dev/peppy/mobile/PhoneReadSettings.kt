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
    enum class State { LOADING, SAVING, ERROR, SMS_SYNC_REQUIRED, SMS_READ_REQUIRED, OFF_READY, WORKING, ACCESS_LIMITED, OUTBOUND_UNSUPPORTED, SMS_SYNC_PAUSED, SMS_READ_PAUSED }
}

internal data class PhoneReadReadback(val enabled: Boolean?, val failed: Boolean)

internal interface PhoneReadSettingsStore {
    fun read(): Pair<Boolean?, Boolean?>
    fun write(value: Boolean): Boolean?
}

private class GatewayPhoneReadSettingsStore(context: Context) : PhoneReadSettingsStore {
    private val host = GatewayPolicyHost(context)

    override fun read() = host.phoneReadEnabledOrNull() to host.smsCaptureEnabled

    override fun write(value: Boolean): Boolean? {
        host.setPhoneReadEnabled(value)
        return host.phoneReadEnabledOrNull()
    }
}

internal fun phoneReadReadback(lastKnown: Boolean?, requested: Boolean?, readBack: Boolean?): PhoneReadReadback =
    if (readBack != null && (requested == null || readBack == requested)) PhoneReadReadback(readBack, false)
    else PhoneReadReadback(lastKnown, true)

internal fun phoneReadOperationIsCurrent(operation: Int, current: Int): Boolean = operation == current

internal fun phoneReadPresentation(
    enabled: Boolean?,
    smsSyncEnabled: Boolean,
    smsReadAccess: Boolean,
    notificationAccess: Boolean,
    outboundSupported: Boolean,
    loading: Boolean = false,
    saving: Boolean = false,
    failed: Boolean = false,
    accessDenied: Boolean = false,
): PhoneReadPresentation = when {
    enabled == null && loading -> PhoneReadPresentation(PhoneReadPresentation.State.LOADING, false)
    enabled == null || failed -> PhoneReadPresentation(PhoneReadPresentation.State.ERROR, false, enabled == true && outboundSupported && !notificationAccess)
    saving -> PhoneReadPresentation(PhoneReadPresentation.State.SAVING, false, enabled && outboundSupported && !notificationAccess)
    !enabled && !smsSyncEnabled -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_SYNC_REQUIRED, false)
    !enabled && !smsReadAccess -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_READ_REQUIRED, false)
    !enabled -> PhoneReadPresentation(PhoneReadPresentation.State.OFF_READY, true)
    !smsSyncEnabled -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_SYNC_PAUSED, true, showOpenAccess = outboundSupported && !notificationAccess)
    !smsReadAccess -> PhoneReadPresentation(PhoneReadPresentation.State.SMS_READ_PAUSED, true, showOpenAccess = outboundSupported && !notificationAccess)
    !outboundSupported -> PhoneReadPresentation(PhoneReadPresentation.State.OUTBOUND_UNSUPPORTED, true)
    !notificationAccess -> PhoneReadPresentation(PhoneReadPresentation.State.ACCESS_LIMITED, true, showOpenAccess = true)
    else -> PhoneReadPresentation(PhoneReadPresentation.State.WORKING, true)
}

@Composable
internal fun PhoneReadSettings(
    context: Context,
    refresh: Int,
    onChanged: () -> Unit,
    store: PhoneReadSettingsStore = GatewayPhoneReadSettingsStore(context),
    notificationAccessGranted: () -> Boolean = { NotificationMirrorAccess.granted(context) },
) {
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
    val outboundSupported = Build.VERSION.SDK_INT >= Build.VERSION_CODES.P
    val smsReadAccess = ContextCompat.checkSelfPermission(context, android.Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED

    fun load() {
        if (saving) return
        val operation = ++operationGeneration
        if (enabled == null) loading = true
        scope.launch {
            val settings = try {
                withContext(Dispatchers.IO) {
                    store.read()
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
                    store.write(value)
                }
            } catch (_: Exception) {
                null
            }
            if (phoneReadOperationIsCurrent(operation, operationGeneration)) {
                val readback = phoneReadReadback(enabled, requested = value, readBack = stored)
                enabled = readback.enabled
                readbackFailed = readback.failed
                saving = false
                if (!readback.failed) {
                    if (value) {
                        SmsCaptureWork.enqueue(context.applicationContext)
                        SmsCaptureWork.ensurePeriodic(context.applicationContext)
                        MmsCaptureWork.onPermissionOrForeground(context.applicationContext)
                    }
                    onChanged()
                }
                consent = consent.cancel()
            }
        }
    }

    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        notificationAccess = notificationAccessGranted()
        accessDenied = !notificationAccess
    }

    LaunchedEffect(refresh) { load() }
    LaunchedEffect(accessRefresh) {
        notificationAccess = notificationAccessGranted()
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

    val presentation = phoneReadPresentation(enabled, smsSyncEnabled == true, smsReadAccess, notificationAccess, outboundSupported, loading, saving, readbackFailed, accessDenied)
    val subtitle = when (presentation.state) {
        PhoneReadPresentation.State.LOADING -> null
        PhoneReadPresentation.State.SAVING -> stringResource(R.string.peppy_toggle_android_phone_read_saving)
        PhoneReadPresentation.State.ERROR -> stringResource(R.string.peppy_toggle_android_phone_read_error)
        PhoneReadPresentation.State.SMS_SYNC_REQUIRED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_required)
        PhoneReadPresentation.State.SMS_READ_REQUIRED -> stringResource(R.string.peppy_toggle_android_phone_read_sms_read_required)
        PhoneReadPresentation.State.OFF_READY -> stringResource(R.string.peppy_toggle_android_phone_read_off_hint)
        PhoneReadPresentation.State.WORKING -> stringResource(R.string.peppy_toggle_android_phone_read_on_hint)
        PhoneReadPresentation.State.ACCESS_LIMITED -> stringResource(if (accessDenied) R.string.peppy_toggle_android_phone_read_access_denied else R.string.peppy_toggle_android_phone_read_access_limited)
        PhoneReadPresentation.State.OUTBOUND_UNSUPPORTED -> stringResource(R.string.peppy_toggle_android_phone_read_outbound_unsupported)
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
        nextStep = null,
        onDismiss = { consent = consent.cancel() },
        onConfirm = {
            notificationAccess = notificationAccessGranted()
            val transition = consent.agree(hasAccess = true)
            consent = transition.state
            when (transition.effect) {
                ConsentEffect.APPLY -> writeAndRead(true)
                ConsentEffect.NONE -> Unit
                ConsentEffect.REQUEST_ACCESS -> Unit
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
