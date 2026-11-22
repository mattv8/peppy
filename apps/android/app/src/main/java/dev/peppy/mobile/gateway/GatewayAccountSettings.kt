package dev.peppy.mobile.gateway

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.AddToHomeScreen
import androidx.compose.material.icons.automirrored.filled.MenuBook
import androidx.compose.material.icons.filled.Cloud
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.CreditCard
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import kotlinx.coroutines.flow.StateFlow
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalResources
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.R
import dev.peppy.mobile.about.AboutBuildInfo
import dev.peppy.mobile.about.AboutRoute
import dev.peppy.mobile.about.AboutScreen
import dev.peppy.mobile.about.AssetThirdPartyNotices
import dev.peppy.mobile.about.GUIDES_URL
import dev.peppy.mobile.about.LicenseTextScreen
import dev.peppy.mobile.about.LicensesScreen
import dev.peppy.mobile.about.ThirdPartyNotices
import dev.peppy.mobile.account.AccountAccessState
import dev.peppy.mobile.account.AccountSettingsPolicy
import dev.peppy.mobile.account.HOSTED_ACCOUNT_URL
import dev.peppy.mobile.attachments.AttachmentStorageLoad
import dev.peppy.mobile.attachments.AttachmentStorageSettingsController
import dev.peppy.mobile.attachments.AttachmentStorageUiState
import dev.peppy.mobile.attachments.AttachmentStoragePage
import dev.peppy.mobile.ui.BadgeSpec
import dev.peppy.mobile.ui.ExternalLinkRow
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.NavigationRow
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingsGroup
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val HOSTED_ORIGIN = "https://peppy.pro"

/** Drop-in Account destination; the parent renders it on the Account tab only. */
@Composable
internal fun GatewayAccountSettings(
    onChanged: () -> Unit,
    onOpenSettings: () -> Unit = {},
    onAddPhone: () -> Unit = {},
    canAddPhone: Boolean = false,
    onConnectComputer: () -> Unit = {},
    canConnectComputer: Boolean = false,
    onOpenSubscription: () -> Unit = {},
    attachmentStorageOriginSupported: (String?) -> Boolean = { it == HOSTED_ORIGIN },
    accountLoader: (Context) -> GatewayAccountState? = GatewayAccountHost::load,
    gatewayStatusLoader: (Context) -> GatewayStatus = NativeGateway::status,
    attachmentStorageScopeLoader: (Context) -> GatewayAccountHost.AttachmentStorageScope? = GatewayAccountHost::attachmentStorageScope,
    attachmentStorageLoader: (Context, GatewayAccountHost.AttachmentStorageScope) -> AttachmentStorageLoad? = GatewayAccountHost::loadAttachmentStorage,
    thirdPartyNoticesLoader: (Context) -> ThirdPartyNotices? = { AssetThirdPartyNotices.openOrNull(it.assets) },
    readOnlyState: StateFlow<Boolean> = AccountAccessState.readOnly,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    var state by remember { mutableStateOf<GatewayAccountState?>(null) }
    var gatewayStatus by remember { mutableStateOf<GatewayStatus?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var aboutLinkError by remember { mutableStateOf<String?>(null) }
    var refreshing by remember { mutableStateOf(false) }
    var devicesOpen by remember { mutableStateOf(false) }
    var storageOpen by remember { mutableStateOf(false) }
    var aboutRoute by remember { mutableStateOf<AboutRoute>(AboutRoute.Closed) }
    var thirdPartyNotices by remember { mutableStateOf<ThirdPartyNotices?>(null) }
    var storageBusy by remember { mutableStateOf(false) }
    val attachmentStorageController = remember { AttachmentStorageSettingsController() }
    var attachmentStorageScope by remember { mutableStateOf<GatewayAccountHost.AttachmentStorageScope?>(null) }
    var attachmentStorageState by remember { mutableStateOf<AttachmentStorageUiState>(AttachmentStorageUiState.Loading) }
    val scope = rememberCoroutineScope()
    suspend fun refreshStorage() {
        if (storageBusy) return
        storageBusy = true
        try {
            val storageScope = withContext(Dispatchers.IO) { attachmentStorageScopeLoader(context) }
            if (storageScope == null) {
                attachmentStorageScope = null
                attachmentStorageController.begin("")
                attachmentStorageState = AttachmentStorageUiState.Unavailable(null)
                return
            }
            attachmentStorageScope = storageScope
            attachmentStorageController.begin(storageScope.value)
            attachmentStorageState = attachmentStorageController.state
            val storage = withContext(Dispatchers.IO) { attachmentStorageLoader(context, storageScope) }
            if (storage != null && attachmentStorageController.apply(storageScope.value, storage)) {
                attachmentStorageState = attachmentStorageController.state
            } else if (attachmentStorageScope?.value == storageScope.value) {
                attachmentStorageState = AttachmentStorageUiState.Unavailable(null)
            }
        } finally {
            storageBusy = false
        }
    }
    fun closeStorageDetail() {
        storageOpen = false
        attachmentStorageScope = null
        attachmentStorageController.begin("")
        attachmentStorageState = AttachmentStorageUiState.Loading
    }
    suspend fun refresh() {
        val loaded = withContext(Dispatchers.IO) { accountLoader(context) }
        gatewayStatus = withContext(Dispatchers.IO) { gatewayStatusLoader(context) }
        state = loaded
        if (state == null) failure = resources.getString(R.string.peppy_devices_load_failed)
    }
    LaunchedEffect(Unit) { refresh() }
    LaunchedEffect(Unit) { thirdPartyNotices = withContext(Dispatchers.IO) { thirdPartyNoticesLoader(context) } }
    LaunchedEffect(storageOpen) { if (storageOpen) refreshStorage() }
    val aboutOpen = aboutRoute != AboutRoute.Closed
    val displayedAboutRoute = when (val route = aboutRoute) {
        AboutRoute.Licenses -> if (thirdPartyNotices == null) AboutRoute.About else route
        is AboutRoute.License -> if (thirdPartyNotices?.entries()?.any { it.fileName == route.fileName } == true) route else AboutRoute.About
        else -> route
    }
    LaunchedEffect(aboutRoute) {
        if (aboutRoute != AboutRoute.Closed) aboutLinkError = null
    }
    LaunchedEffect(aboutRoute, displayedAboutRoute) {
        if (aboutRoute != displayedAboutRoute) aboutRoute = displayedAboutRoute
    }
    // Lock and recovery may change while this screen remains open. This is local state only; the
    // explicit Refresh button remains the only roster/server request.
    LaunchedEffect(Unit) {
        while (true) {
            gatewayStatus = withContext(Dispatchers.IO) { gatewayStatusLoader(context) }
            delay(1_000)
        }
    }

    val accountReadOnly by readOnlyState.collectAsState()
    val account = state
    val origin = gatewayStatus?.origin
    val storageSupported = attachmentStorageOriginSupported(origin)
    LaunchedEffect(state == null) {
        if (state == null) {
            devicesOpen = false
            if (storageOpen) closeStorageDetail()
        }
    }

    Column(Modifier.fillMaxWidth().testTag("account-tab-screen")) {
        if (aboutOpen) {
            when (val route = displayedAboutRoute) {
                AboutRoute.About -> AboutScreen(
                    buildInfo = AboutBuildInfo.fromBuildConfig(),
                    onBack = { aboutRoute = AboutRoute.Closed },
                    onOpenLink = { openUri(context, it, failureSetter = { error -> aboutLinkError = error }) },
                    onOpenLicenses = { aboutRoute = AboutRoute.Licenses },
                    licensesAvailable = thirdPartyNotices != null,
                    linkError = aboutLinkError,
                )
                AboutRoute.Licenses -> thirdPartyNotices?.let { notices ->
                    LicensesScreen(notices, onBack = { aboutRoute = AboutRoute.About }, onOpenLicense = { aboutRoute = AboutRoute.License(it.fileName) })
                }
                is AboutRoute.License -> {
                    val notices = thirdPartyNotices
                    val entry = notices?.entries()?.firstOrNull { it.fileName == route.fileName }
                    if (notices != null && entry != null) LicenseTextScreen(notices, entry, onBack = { aboutRoute = AboutRoute.Licenses })
                }
                AboutRoute.Closed -> Unit
            }
        } else if (account == null) {
            HeroBanner(
                stringResource(R.string.peppy_toggle_android_account_offline_title),
                origin ?: stringResource(R.string.peppy_toggle_android_account_offline_body),
                "account-hero",
            )
            ScreenBody("account-offline-body") {
                Text(failure ?: stringResource(R.string.peppy_toggle_android_account_loading), Modifier.testTag("devices-loading"))
                TextButton(onClick = { scope.launch { failure = null; refresh() } }, modifier = Modifier.testTag("account-retry-button")) {
                    Text(stringResource(R.string.peppy_toggle_android_retry))
                }
                AppGroup(
                    onOpenSettings = onOpenSettings,
                    onOpenGuides = { openUri(context, GUIDES_URL, failureSetter = { failure = it }) },
                    onOpenAbout = { aboutRoute = AboutRoute.About },
                )
            }
        } else if (devicesOpen) {
            DevicesPage(
                state = account,
                onBack = { devicesOpen = false },
                onRefresh = {
                    scope.launch {
                        failure = null
                        refreshing = true
                        try {
                            refresh()
                        } finally {
                            refreshing = false
                        }
                        onChanged()
                    }
                },
                refreshing = refreshing,
                revoke = { device -> withContext(Dispatchers.IO) { GatewayAccountHost.revoke(context, device.id) } },
                rename = { device, name -> withContext(Dispatchers.IO) { GatewayAccountHost.rename(context, device.id, name) } },
                onChanged = onChanged,
                accountReadOnly = accountReadOnly,
                errorMessage = failure,
            )
        } else if (storageOpen && storageSupported) {
            AttachmentStoragePage(
                state = attachmentStorageState,
                scope = attachmentStorageScope?.value,
                busy = storageBusy,
                onBack = ::closeStorageDetail,
                onRefresh = { scope.launch { refreshStorage() } },
                onManage = { manageOrigin -> openUri(context, "$manageOrigin/account", failureSetter = { failure = it }) },
                showPurchaseActions = BuildConfig.PEPPY_DISTRIBUTION != "play",
            )
            AccountError(failure)
        } else {
            HeroBanner(
                title = stringResource(R.string.peppy_account),
                body = origin.orEmpty(),
                tag = "account-hero",
                badges = listOf(BadgeSpec(roleLabel(account.self.role), "account-hero-role")),
            )
            ScreenBody("account-body") {
                SettingsGroup(tag = "account-section", title = stringResource(R.string.peppy_devices), showDivider = false) {
                    NavigationRow(
                        tag = "account-devices-row",
                        label = stringResource(R.string.peppy_toggle_android_account_devices),
                        onClick = { devicesOpen = true },
                        icon = Icons.Default.Devices,
                        summary = stringResource(R.string.peppy_account_devices_active, account.devices.count { !it.revoked }),
                    )
                    if (AccountSettingsPolicy.canAddPhone(account.self.role, canAddPhone)) {
                        NavigationRow(
                            tag = "add-phone-row",
                            label = stringResource(R.string.peppy_toggle_android_account_add_phone),
                            onClick = onAddPhone,
                            icon = Icons.AutoMirrored.Filled.AddToHomeScreen,
                        )
                    }
                    if (canConnectComputer) {
                        NavigationRow(
                            tag = "connect-computer-row",
                            label = stringResource(R.string.peppy_owner_computer_add_cta),
                            onClick = onConnectComputer,
                            icon = Icons.Default.Computer,
                        )
                    }
                }
                val hosted = origin == HOSTED_ORIGIN
                if (hosted || storageSupported) SettingsGroup(tag = "account-plan-group", title = stringResource(R.string.peppy_account_group_plan)) {
                    val attention = if (accountReadOnly) stringResource(R.string.peppy_account_attention_action_needed) else null
                    if (hosted && BuildConfig.PEPPY_DISTRIBUTION == "play") {
                        NavigationRow(
                            tag = "account-subscription-row",
                            label = stringResource(R.string.peppy_play_account_row),
                            onClick = onOpenSubscription,
                            icon = Icons.Default.CreditCard,
                            attention = attention,
                        )
                    } else if (hosted) {
                        ExternalLinkRow(
                            tag = "account-hosted-row",
                            label = stringResource(R.string.peppy_toggle_android_account_billing),
                            onClick = { openUri(context, HOSTED_ACCOUNT_URL, failureSetter = { failure = it }) },
                            icon = Icons.Default.CreditCard,
                            attention = attention,
                        )
                    }
                    if (storageSupported) NavigationRow(
                        tag = "attachment-storage-row",
                        label = stringResource(R.string.peppy_account_storage_row),
                        onClick = { storageOpen = true },
                        icon = Icons.Default.Cloud,
                    )
                }
                AppGroup(
                    onOpenSettings = onOpenSettings,
                    onOpenGuides = { openUri(context, GUIDES_URL, failureSetter = { failure = it }) },
                    onOpenAbout = { aboutRoute = AboutRoute.About },
                )
                AccountError(failure)
            }
        }
    }
}

@Composable
private fun AppGroup(onOpenSettings: () -> Unit, onOpenGuides: () -> Unit, onOpenAbout: () -> Unit) {
    SettingsGroup(tag = "account-app-group", title = stringResource(R.string.peppy_account_group_app)) {
        NavigationRow(
            tag = "account-settings-row",
            label = stringResource(R.string.peppy_toggle_android_account_settings),
            onClick = onOpenSettings,
            icon = Icons.Default.Settings,
        )
        ExternalLinkRow(
            tag = "account-guides-row",
            label = stringResource(R.string.peppy_guides),
            onClick = onOpenGuides,
            icon = Icons.AutoMirrored.Filled.MenuBook,
        )
        NavigationRow(
            tag = "account-about-row",
            label = stringResource(R.string.peppy_about_row_label),
            onClick = onOpenAbout,
            icon = Icons.Default.Info,
        )
    }
}

@Composable
private fun AccountError(message: String?) {
    message?.let {
        Text(
            it,
            Modifier.testTag("account-error").semantics { liveRegion = LiveRegionMode.Polite },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
        )
    }
}

@Composable
private fun roleLabel(role: String): String = when (role) {
    "owner" -> stringResource(R.string.peppy_devices_role_owner)
    "gateway" -> stringResource(R.string.peppy_devices_role_gateway)
    "device" -> stringResource(R.string.peppy_devices_role_device)
    else -> role
}

private fun openUri(context: Context, uri: String, failureSetter: (String) -> Unit) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri)))
    } catch (_: android.content.ActivityNotFoundException) {
        failureSetter(context.getString(R.string.peppy_toggle_android_link_unavailable))
    }
}
