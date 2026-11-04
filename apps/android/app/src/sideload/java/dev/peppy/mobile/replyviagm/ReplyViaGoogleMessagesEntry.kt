package dev.peppy.mobile.replyviagm

import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.GatewayWork
import dev.peppy.mobile.NotificationMirrorAccess
import dev.peppy.mobile.R
import dev.peppy.mobile.Section
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val GOOGLE_MESSAGES_PACKAGE = "com.google.android.apps.messaging"

@Composable
internal fun ReplyViaGoogleMessagesEntry(refresh: Int, onChanged: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val preferences = remember { ReplyViaGmPreferences(context) }
    var enabled by remember { mutableStateOf<Boolean?>(null) }
    var accessGranted by remember { mutableStateOf(false) }
    var googleMessagesInstalled by remember { mutableStateOf<Boolean?>(null) }
    var consent by remember { mutableStateOf(ConsentGatedToggle()) }
    var foregroundRefresh by remember { mutableIntStateOf(0) }

    fun reload() {
        scope.launch {
            accessGranted = withContext(Dispatchers.IO) { NotificationMirrorAccess.granted(context) }
            googleMessagesInstalled = withContext(Dispatchers.IO) { googleMessagesInstalled(context) }
            enabled = withContext(Dispatchers.IO) { preferences.enabled() }
        }
    }

    fun applyEnabled(value: Boolean) {
        enabled = null
        scope.launch {
            val persisted = withContext(Dispatchers.IO) {
                var readback = false
                try {
                    runCatching {
                        if (value && !NotificationMirrorAccess.granted(context)) return@runCatching
                        preferences.setEnabled(value)
                    }
                } finally {
                    readback = runCatching { preferences.enabled() }.getOrDefault(false)
                }
                readback
            }
            enabled = persisted
            consent = consent.cancel()
            if (persisted) GatewayWork.enqueue(context)
            onChanged()
        }
    }

    fun consumeAccessResult(granted: Boolean) {
        val transition = consent.accessResult(granted)
        consent = transition.state
        if (transition.effect == ConsentEffect.APPLY) applyEnabled(true)
    }

    val accessLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        scope.launch {
            accessGranted = withContext(Dispatchers.IO) { NotificationMirrorAccess.granted(context) }
            consumeAccessResult(accessGranted)
            onChanged()
        }
    }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) foregroundRefresh++ }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    DisposableEffect(Unit) { onDispose { consent = consent.cancel() } }
    LaunchedEffect(refresh, foregroundRefresh) { reload() }

    ReplyViaGoogleMessagesSettings(
        enabled = enabled,
        accessGranted = accessGranted,
        googleMessagesInstalled = googleMessagesInstalled,
        consent = consent,
        onConsentChanged = { consent = it },
        onApplyEnabled = ::applyEnabled,
        onRequestAccess = { accessLauncher.launch(NotificationMirrorAccess.settingsIntent()) },
    )
}

@Composable
internal fun ReplyViaGoogleMessagesSettings(
    enabled: Boolean?,
    accessGranted: Boolean,
    googleMessagesInstalled: Boolean?,
    consent: ConsentGatedToggle,
    onConsentChanged: (ConsentGatedToggle) -> Unit,
    onApplyEnabled: (Boolean) -> Unit,
    onRequestAccess: () -> Unit,
) {
    val checked = enabled == true
    val installed = googleMessagesInstalled
    val rowEnabled = enabled != null && installed != null && consent.phase == ConsentPhase.IDLE && (installed || checked)
    val subtitle = when {
        enabled == null || installed == null -> null
        !checked && !installed -> stringResource(R.string.peppy_toggle_android_reply_via_gm_not_installed_hint)
        !checked && !accessGranted -> stringResource(R.string.peppy_toggle_android_reply_via_gm_access_hint)
        !checked -> stringResource(R.string.peppy_toggle_android_reply_via_gm_hint_off)
        !accessGranted -> stringResource(R.string.peppy_toggle_android_reply_via_gm_paused_access_hint)
        !installed -> stringResource(R.string.peppy_toggle_android_reply_via_gm_paused_missing_hint)
        else -> stringResource(R.string.peppy_toggle_android_reply_via_gm_hint_on)
    }
    val stateDescription = when {
        enabled == null || installed == null -> null
        !checked && !installed -> stringResource(R.string.peppy_toggle_android_reply_via_gm_state_off_unavailable)
        !checked && !accessGranted -> stringResource(R.string.peppy_toggle_android_reply_via_gm_state_off_access)
        !checked -> stringResource(R.string.peppy_toggle_android_reply_via_gm_state_off)
        !accessGranted || !installed -> stringResource(R.string.peppy_toggle_android_reply_via_gm_state_on_paused)
        else -> stringResource(R.string.peppy_toggle_android_reply_via_gm_state_on)
    }

    Section(
        "reply-via-gm-section",
        stringResource(R.string.peppy_toggle_android_reply_via_gm_label),
        helpBody = stringResource(R.string.peppy_help_android_reply_via_gm),
    ) {
        SettingToggleRow(
            title = stringResource(R.string.peppy_toggle_android_reply_via_gm_label),
            checked = checked,
            enabled = rowEnabled,
            onCheckedChange = { requested -> if (requested) onConsentChanged(consent.requestEnable()) else onApplyEnabled(false) },
            tag = "reply-via-gm-row",
            subtitle = subtitle,
            onClickLabel = if (!checked && rowEnabled) stringResource(R.string.peppy_toggle_android_reply_via_gm_review) else null,
            stateDescription = stateDescription,
        )
    }

    if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_reply_via_gm_consent_title),
        body = listOf(
            stringResource(R.string.peppy_reply_via_gm_consent_p_how),
            stringResource(R.string.peppy_reply_via_gm_consent_p_scope),
            stringResource(R.string.peppy_reply_via_gm_consent_p_unknown),
            stringResource(R.string.peppy_reply_via_gm_consent_p_read),
            stringResource(R.string.peppy_reply_via_gm_consent_p_access),
            stringResource(R.string.peppy_reply_via_gm_consent_p_optin),
        ).joinToString("\n\n"),
        tag = "reply-via-gm-consent-sheet",
        disclosureTag = "reply-via-gm-disclosure",
        showPrivacyPolicy = false,
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        dismissLabel = stringResource(R.string.peppy_toggle_android_not_now),
        nextStep = if (accessGranted) null else stringResource(R.string.peppy_toggle_android_mirroring_next_access),
        onDismiss = { onConsentChanged(consent.cancel()) },
        onConfirm = {
            val transition = replyViaGmConsentTransition(consent, accessGranted)
            onConsentChanged(transition.state)
            when (transition.effect) {
                ConsentEffect.APPLY -> onApplyEnabled(true)
                ConsentEffect.REQUEST_ACCESS -> onRequestAccess()
                ConsentEffect.NONE -> Unit
            }
        },
    )
}

internal fun replyViaGmConsentTransition(consent: ConsentGatedToggle, accessGranted: Boolean) = consent.agree(accessGranted)

private fun googleMessagesInstalled(context: Context): Boolean = try {
    context.packageManager.getApplicationInfo(GOOGLE_MESSAGES_PACKAGE, 0)
    true
} catch (_: PackageManager.NameNotFoundException) {
    false
}
