package dev.peppy.mobile.settings

import android.Manifest
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CreditCard
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.R
import dev.peppy.mobile.RelayWakePreference
import dev.peppy.mobile.RelayWakeRuntime
import dev.peppy.mobile.RelayWakeStatus
import dev.peppy.mobile.RemoteMessagingPreferences
import dev.peppy.mobile.RemoteMessagingRuntime
import dev.peppy.mobile.RemoteMessagingStatus
import dev.peppy.mobile.billing.PlayDeletionContext
import dev.peppy.mobile.gateway.GatewayAccountHost
import dev.peppy.mobile.gateway.GatewayAccountState
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.gateway.SyncPhase
import dev.peppy.mobile.ui.DetailPage
import dev.peppy.mobile.ui.NavigationRow
import dev.peppy.mobile.ui.PageChrome
import dev.peppy.mobile.ui.SettingsGroup
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.hostedWakeRelayUrl

internal enum class SettingsRoute { Root, Advanced }

private const val HOSTED_ORIGIN = "https://peppy.pro"
private const val LIVE_STATUS_POLL_MS = 1_000L
private const val RELAY_PREFS = "peppy-relay"
internal const val RELAY_ENABLED = "relay-enabled.v1"
private const val RELAY_ORIGIN = "relay-origin.v1"
private const val WAKE_PREFERENCE = "hosted-relay-preference.v1"
private const val WAKE_STATUS = "hosted-relay-status.v1"

private fun postNotificationsGranted(context: Context): Boolean =
    Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

/** Every value the Settings page reads. Writes happen only on the IO dispatcher or from the poll, never on a render. */
internal class SettingsState(context: Context, status: GatewayStatus?) {
    val relayPrefs: SharedPreferences = context.getSharedPreferences(RELAY_PREFS, 0)
    var gatewayStatus: GatewayStatus? by mutableStateOf(status)
    var account: GatewayAccountState? by mutableStateOf(null)
    var accountLoaded: Boolean by mutableStateOf(false)
    var failure: String? by mutableStateOf(null)
    var notificationsGranted: Boolean by mutableStateOf(postNotificationsGranted(context))
    var liveSyncStatus: RemoteMessagingStatus by mutableStateOf(RemoteMessagingRuntime.status())
    var remoteMessagingEnabled: Boolean by mutableStateOf(RemoteMessagingPreferences.enabled(context))
    var notificationConsentOpen: Boolean by mutableStateOf(false)
    var wakePreference: RelayWakePreference? by mutableStateOf(null)
    var wakeStatus: RelayWakeStatus by mutableStateOf(RelayWakeStatus.PENDING)
    var relayLoaded: Boolean by mutableStateOf(false)
    var relayWritePending: Boolean by mutableStateOf(false)
    var relayEnabled: Boolean by mutableStateOf(relayPrefs.getBoolean(RELAY_ENABLED, false))
    var relayOrigin: String by mutableStateOf(relayPrefs.getString(RELAY_ORIGIN, "") ?: "")
    var relayOriginInvalid: Boolean by mutableStateOf(false)
    var vaultDeleteInFlight: Boolean by mutableStateOf(false)

    val keysReady: Boolean get() = gatewayStatus?.sharedKeysReady == true
    val origin: String? get() = gatewayStatus?.origin
    val hosted: Boolean get() = origin == HOSTED_ORIGIN
    val role: String? get() = account?.self?.role
    val liveSyncRole: Boolean get() = role == "owner" || role == "gateway"
    val ownerRole: Boolean get() = keysReady && role == "owner"
    val accountOffline: Boolean get() = keysReady && accountLoaded && account == null
    val liveSyncShown: Boolean get() = keysReady && (!accountLoaded || liveSyncRole)
    val liveSyncEnabled: Boolean get() = keysReady && accountLoaded && gatewayStatus?.syncPhase != SyncPhase.RECOVERY_REQUIRED
    val backgroundVisible: Boolean get() = keysReady && origin?.let { hostedWakeRelayUrl(it) } != null
    val syncGroupShown: Boolean get() = liveSyncShown || backgroundVisible || accountOffline
    val relayInAdvanced: Boolean get() = keysReady && gatewayStatus?.enrolled == true && origin != null && !backgroundVisible
    val relayReady: Boolean get() = relayLoaded && gatewayStatus != null && !relayWritePending

    fun refreshLiveValues(context: Context) {
        notificationsGranted = postNotificationsGranted(context)
        liveSyncStatus = RemoteMessagingRuntime.status()
    }
}

/**
 * Settings root and its Advanced route. The gateway status poll, roster load and wake relay state
 * live here so both routes share them. Blocking calls run on Dispatchers.IO.
 */
@Composable
internal fun SettingsScreen(
    status: GatewayStatus?,
    isPlay: Boolean,
    playDeletionContext: PlayDeletionContext,
    onClose: () -> Unit,
    onOpenSubscription: () -> Unit,
    onChanged: () -> Unit,
    accountLoader: (Context) -> GatewayAccountState? = GatewayAccountHost::load,
    gatewayStatusLoader: (Context) -> GatewayStatus = NativeGateway::status,
    vaultDeleter: (Context) -> Boolean = GatewayAccountHost::deleteVault,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val state = remember { SettingsState(context, status) }
    var route by remember { mutableStateOf(SettingsRoute.Root) }

    LaunchedEffect(Unit) {
        while (true) {
            state.gatewayStatus = withContext(Dispatchers.IO) { gatewayStatusLoader(context) }
            state.refreshLiveValues(context)
            delay(LIVE_STATUS_POLL_MS)
        }
    }
    LaunchedEffect(state.keysReady) {
        if (!state.keysReady) {
            state.account = null
            state.accountLoaded = false
        } else if (!state.accountLoaded) {
            state.account = withContext(Dispatchers.IO) { accountLoader(context) }
            state.accountLoaded = true
        }
    }
    LaunchedEffect(Unit) {
        state.wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) }
        state.wakeStatus = withContext(Dispatchers.IO) { RelayWakeRuntime.status(context) }
        state.relayLoaded = true
    }
    DisposableEffect(state.relayPrefs) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            when (key) {
                WAKE_PREFERENCE -> scope.launch { state.wakePreference = withContext(Dispatchers.IO) { RelayWakeRuntime.preference(context) } }
                WAKE_STATUS -> scope.launch { state.wakeStatus = withContext(Dispatchers.IO) { RelayWakeRuntime.status(context) } }
                RELAY_ENABLED -> state.relayEnabled = state.relayPrefs.getBoolean(RELAY_ENABLED, false)
                RELAY_ORIGIN -> state.relayOrigin = state.relayPrefs.getString(RELAY_ORIGIN, "") ?: ""
            }
        }
        state.relayPrefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { state.relayPrefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) state.refreshLiveValues(context)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val vaultDeleteFailed = stringResource(R.string.peppy_vault_delete_failed)
    fun submitVaultDelete() {
        if (!state.ownerRole || state.vaultDeleteInFlight) return
        state.vaultDeleteInFlight = true
        scope.launch {
            try {
                val deleted = withContext(Dispatchers.IO) { vaultDeleter(context) }
                if (deleted) onChanged() else state.failure = vaultDeleteFailed
            } finally {
                state.vaultDeleteInFlight = false
            }
        }
    }

    when (route) {
        SettingsRoute.Root -> SettingsRootPage(
            state = state,
            isPlay = isPlay,
            playDeletionContext = playDeletionContext,
            onClose = onClose,
            onOpenSubscription = onOpenSubscription,
            onOpenAdvanced = { route = SettingsRoute.Advanced },
            onSubmitVaultDelete = { submitVaultDelete() },
        )
        SettingsRoute.Advanced -> AdvancedSettingsPage(
            state = state,
            onBack = { route = SettingsRoute.Root },
        )
    }
}

@Composable
private fun SettingsRootPage(
    state: SettingsState,
    isPlay: Boolean,
    playDeletionContext: PlayDeletionContext,
    onClose: () -> Unit,
    onOpenSubscription: () -> Unit,
    onOpenAdvanced: () -> Unit,
    onSubmitVaultDelete: () -> Unit,
) {
    DetailPage(PageChrome(title = stringResource(R.string.peppy_settings), tag = "settings", onBack = onClose)) {
        val syncShown = state.syncGroupShown
        val planShown = isPlay && state.hosted && (!state.keysReady || (state.accountLoaded && state.account == null))
        if (syncShown) SyncSettingsGroup(state)
        if (planShown) SettingsGroup(
            tag = "settings-account-group",
            title = stringResource(R.string.peppy_settings_group_plan),
            showDivider = syncShown,
        ) {
            NavigationRow(
                tag = "settings-subscription-row",
                label = stringResource(R.string.peppy_play_account_row),
                onClick = onOpenSubscription,
                icon = Icons.Default.CreditCard,
            )
        }
        SettingsGroup(tag = "settings-advanced-group", title = null, showDivider = syncShown || planShown) {
            NavigationRow(
                tag = "settings-advanced-row",
                label = stringResource(R.string.peppy_settings_advanced),
                onClick = onOpenAdvanced,
                icon = Icons.Default.Tune,
                summary = stringResource(
                    if (state.relayInAdvanced) R.string.peppy_settings_advanced_summary_relay else R.string.peppy_settings_advanced_summary,
                ),
            )
        }
        if (state.hosted || state.ownerRole || state.vaultDeleteInFlight) DangerZone(
            state = state,
            isPlay = isPlay,
            playDeletionContext = playDeletionContext,
            onOpenSubscription = onOpenSubscription,
            onSubmitVaultDelete = onSubmitVaultDelete,
        )
        state.failure?.let {
            Text(
                it,
                Modifier
                    .testTag("account-error")
                    .semantics { liveRegion = LiveRegionMode.Polite },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}
