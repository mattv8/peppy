package dev.peppy.mobile.googlemessages.ui

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.text.format.DateUtils
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.R
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.googlemessages.GoogleMessagesLinkService
import dev.peppy.mobile.googlemessages.LinkOpenResult
import dev.peppy.mobile.googlemessages.LinkCoreBridge
import dev.peppy.mobile.googlemessages.LinkObservation
import dev.peppy.mobile.googlemessages.LinkOperationLock
import dev.peppy.mobile.googlemessages.LinkRuntime
import dev.peppy.mobile.googlemessages.LinkSignInActivity
import dev.peppy.mobile.googlemessages.LinkStatus
import dev.peppy.mobile.googlemessages.TypingSharingPreferences
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.gateway.NativeGateway
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal interface GoogleMessagesLinkPanelHost {
    val changes: StateFlow<Long>
    val emoji: StateFlow<String?>
    val permissionResults: StateFlow<Long>
    val notificationsGranted: Boolean
    val isChangingConfigurations: Boolean
    fun open(): LinkOpenResult
    fun recordPreStart(): Boolean = true
    fun recordPreStart(token: Long): Boolean = recordPreStart()
    fun acquirePairingLease(token: Long): dev.peppy.mobile.googlemessages.LinkHandleLease? =
        (open() as? LinkOpenResult.Open)?.handle?.let { dev.peppy.mobile.googlemessages.LinkHandleLease(it, 0) }
    fun captureStatusObservation(handle: dev.peppy.mobile.googlemessages.LinkHandle): LinkObservation? = null
    fun recordStatusObservation(observation: LinkObservation, status: LinkStatus): Boolean = true
    fun beginOwnerUnlink() = Unit
    fun recordUnlinked(): Boolean = true
    fun recordExplicitUnlinkCompleted(): Boolean = true
    fun resetLocalState()
    fun pairingInProgress(): Boolean
    fun tryBeginPairing(): Long?
    fun endPairing(token: Long)
    fun requestNotificationPermission()
    fun launchSignIn(onResult: (String?) -> Unit)
    fun startService()
    fun typingSharing(): Boolean = true
    fun setTypingSharing(enabled: Boolean): Boolean = false
}

private enum class PendingNotificationAction { LINK, RELINK, NOTIFICATIONS_ONLY }

@Composable
internal fun GoogleMessagesLinkPanel(onChanged: () -> Unit) = GoogleMessagesLinkPanelContent(null, onChanged)

@Composable
internal fun GoogleMessagesLinkPanel(
    injectedHost: GoogleMessagesLinkPanelHost,
    onChanged: () -> Unit,
) = GoogleMessagesLinkPanelContent(injectedHost, onChanged)

@Composable
private fun GoogleMessagesLinkPanelContent(
    injectedHost: GoogleMessagesLinkPanelHost?,
    onChanged: () -> Unit,
) {
    val context = LocalContext.current
    val activity = LocalActivity.current
    if (!GoogleMessagesLink.available(context)) return
    val scope = rememberCoroutineScope()
    val owner = LocalLifecycleOwner.current
    var resume by remember { mutableIntStateOf(0) }
    var result by remember { mutableStateOf<LinkOpenResult?>(null) }
    var status by remember { mutableStateOf<LinkStatus?>(null) }
    var loadingFailed by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var checkResult by remember { mutableStateOf<Int?>(null) }
    var sheet by rememberSaveable { mutableStateOf<String?>(null) }
    var dismissedSeq by rememberSaveable { mutableStateOf<Long?>(null) }
    var localNotice by rememberSaveable { mutableStateOf<String?>(null) }
    var batteryUnavailable by remember { mutableStateOf(false) }
    var batteryRefresh by remember { mutableIntStateOf(0) }
    var typingShare by remember { mutableStateOf<Boolean?>(null) }
    var typingWriteBusy by remember { mutableStateOf(false) }
    var typingError by remember { mutableStateOf(false) }
    var notificationAsked by rememberSaveable { mutableStateOf(false) }
    var refreshGeneration by remember { mutableIntStateOf(0) }
    var refreshJob by remember { mutableStateOf<Job?>(null) }
    var pendingNotificationAction by rememberSaveable { mutableStateOf<PendingNotificationAction?>(null) }
    var signInContinuation by remember { mutableStateOf<((String?) -> Unit)?>(null) }
    val platformPermissionResults = remember { MutableStateFlow(0L) }
    val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        platformPermissionResults.value++
    }
    val signInLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { outcome ->
        signInContinuation?.also { signInContinuation = null }
            ?.invoke(outcome.data?.getStringExtra("dev.peppy.mobile.googlemessages.outcome"))
    }
    val host = injectedHost ?: remember(context, activity) {
        AndroidGoogleMessagesLinkPanelHost(
            context = context,
            changingConfigurations = { activity?.isChangingConfigurations == true },
            permissionResults = platformPermissionResults,
            requestPermission = { permissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS) },
            launchSignIn = { onResult ->
                signInContinuation = onResult
                signInLauncher.launch(Intent(context, LinkSignInActivity::class.java))
            },
        )
    }
    val changes by host.changes.collectAsState()
    val emoji by host.emoji.collectAsState()
    val permissionResult by host.permissionResults.collectAsState()
    val destructiveOperationInProgress by LinkOperationLock.inProgress.collectAsState()

    suspend fun reloadAuthoritativeStatus(): LinkStatus? {
        val opened = withContext(Dispatchers.IO) { host.open() }
        val observation = (opened as? LinkOpenResult.Open)?.let { open ->
            withContext(Dispatchers.IO) { host.captureStatusObservation(open.handle) }
        }
        val loadedStatus = (opened as? LinkOpenResult.Open)?.handle?.let { handle ->
            withContext(Dispatchers.IO) { runCatching { LinkStatus.parse(handle.status()) }.getOrNull() }
        }
        result = opened
        status = loadedStatus
        if (observation != null && loadedStatus != null) {
            withContext(Dispatchers.IO) { host.recordStatusObservation(observation, loadedStatus) }
        }
        loadingFailed = opened is LinkOpenResult.Open && (loadedStatus == null || loadedStatus.state == "unknown")
        return loadedStatus
    }

    fun reload() {
        val request = ++refreshGeneration
        refreshJob?.cancel()
        refreshJob = scope.launch {
            loadingFailed = false
            try {
                val opened = withContext(Dispatchers.IO) { host.open() }
                val observation = (opened as? LinkOpenResult.Open)?.let { open ->
                    withContext(Dispatchers.IO) { host.captureStatusObservation(open.handle) }
                }
                val loadedStatus = (opened as? LinkOpenResult.Open)?.handle?.let { handle ->
                    withContext(Dispatchers.IO) { runCatching { LinkStatus.parse(handle.status()) }.getOrNull() }
                }
                if (observation != null && loadedStatus != null) {
                    withContext(Dispatchers.IO) { host.recordStatusObservation(observation, loadedStatus) }
                }
                if (request != refreshGeneration) return@launch
                if (opened is LinkOpenResult.Open && (loadedStatus == null || loadedStatus.state == "unknown")) loadingFailed = true
                else { result = opened; status = loadedStatus }
            } catch (error: kotlinx.coroutines.CancellationException) { throw error }
            catch (_: Exception) { if (request == refreshGeneration) loadingFailed = true }
        }
    }
    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                resume++
                batteryRefresh++
            } else if (event == Lifecycle.Event.ON_STOP && !host.isChangingConfigurations) {
                pendingNotificationAction = null
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose {
            refreshGeneration++
            refreshJob?.cancel()
            owner.lifecycle.removeObserver(observer)
        }
    }
    LaunchedEffect(changes, resume) { reload() }
    LaunchedEffect(status?.state) {
        if (status?.state == "linked") typingShare = withContext(Dispatchers.IO) { host.typingSharing() }
    }

    fun refreshAfterAction() { resume++; onChanged() }
    fun launchSignIn() {
        host.launchSignIn { outcome ->
            localNotice = when (outcome) {
                "blocked" -> "signin_blocked"
                "cookies_missing", "error" -> "signin_incomplete"
                else -> null
            }
            resume++
        }
    }
    fun continueRelink() {
        scope.launch {
            var pairingToken: Long? = null
            try {
                withContext(NonCancellable + Dispatchers.IO) {
                    pairingToken = host.tryBeginPairing()
                }
                val token = pairingToken ?: return@launch
                busy = true
                val lease = withContext(Dispatchers.IO) { host.acquirePairingLease(token) } ?: return@launch
                if (!withContext(Dispatchers.IO) { host.recordPreStart(token) }) {
                    localNotice = "persist_failed"
                    return@launch
                }
                host.startService()
                withContext(Dispatchers.IO) { lease.handle.relink() }
            }
            catch (error: kotlinx.coroutines.CancellationException) { throw error }
            catch (_: Exception) { localNotice = "relink_failed" }
            finally {
                pairingToken?.let { token ->
                    withContext(NonCancellable + Dispatchers.IO) { host.endPairing(token) }
                    busy = false
                    refreshAfterAction()
                }
            }
        }
    }
    fun continueNotificationAction() {
        val action = pendingNotificationAction ?: return
        pendingNotificationAction = null
        when (action) {
            PendingNotificationAction.LINK -> launchSignIn()
            PendingNotificationAction.RELINK -> continueRelink()
            PendingNotificationAction.NOTIFICATIONS_ONLY -> Unit
        }
    }
    fun requestNotificationThen(action: PendingNotificationAction) {
        if (busy || destructiveOperationInProgress || pendingNotificationAction != null) return
        pendingNotificationAction = action
        if (host.notificationsGranted) continueNotificationAction()
        else host.requestNotificationPermission()
    }
    LaunchedEffect(permissionResult) {
        if (permissionResult > 0L) continueNotificationAction()
    }
    fun requestNotificationsThenSignIn() = requestNotificationThen(PendingNotificationAction.LINK)
    val notificationsOff = !host.notificationsGranted
    val batteryAllowed = batteryRefresh.let { context.getSystemService(PowerManager::class.java)?.isIgnoringBatteryOptimizations(context.packageName) == true }
    fun openBatterySettings() {
        try { context.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
        catch (_: ActivityNotFoundException) { batteryUnavailable = true }
    }
    if (result is LinkOpenResult.Unavailable) return

    Column(Modifier.fillMaxWidth().testTag("google-messages-link-entry")) {
        HorizontalDivider()
        Column(Modifier.fillMaxWidth().testTag("google-messages-link-panel")) {
            Text(stringResource(R.string.peppy_google_messages_link_title), Modifier.semantics { heading() }.testTag("google-messages-link-heading"), style = MaterialTheme.typography.titleMedium)
            when {
                result == null && !loadingFailed -> Loading()
                loadingFailed -> {
                    Text(stringResource(R.string.peppy_google_messages_link_load_failed), Modifier.semantics { liveRegion = LiveRegionMode.Assertive }.testTag("google-messages-link-load-failed"))
                    TextButton(onClick = ::reload, modifier = Modifier.testTag("google-messages-link-reload")) { Text(stringResource(R.string.peppy_try_again)) }
                }
                result is LinkOpenResult.Unreadable -> UnreadablePanel(
                    onReset = { sheet = "reset" },
                    busy = busy || destructiveOperationInProgress,
                    localNotice = localNotice,
                    onDismissNotice = { localNotice = null },
                )
                status != null -> StatusPanel(
                    status = checkNotNull(status), emoji = emoji, busy = busy || destructiveOperationInProgress, checkResult = checkResult,
                    notificationsOff = notificationsOff, batteryAllowed = batteryAllowed, batteryUnavailable = batteryUnavailable,
                    signInBlocked = GoogleMessagesLink.signInBlocked(context), pairingInProgress = host.pairingInProgress(), localNotice = localNotice,
                    dismissedSeq = dismissedSeq, onDismissNotice = { dismissedSeq = status?.seq; localNotice = null },
                    onStart = { sheet = "consent" }, onRelink = { sheet = "relink" }, onUnlink = { sheet = "unlink" },
                    onRemove = { sheet = "remove" }, onCheck = {
                        val handle = (result as? LinkOpenResult.Open)?.handle
                        if (handle != null) {
                            busy = true
                            scope.launch {
                            try { checkResult = withContext(Dispatchers.IO) { handle.checkHealth().toInt() } }
                            catch (error: kotlinx.coroutines.CancellationException) { throw error }
                            catch (_: Exception) { checkResult = -1 }
                            finally { busy = false; refreshAfterAction() }
                            }
                        }
                    }, onReconnect = { host.startService(); refreshAfterAction() },
                    onBattery = { if (batteryAllowed) openBatterySettings() else sheet = "battery" },
                    onAllowNotifications = {
                        if (Build.VERSION.SDK_INT >= 33) {
                            if (notificationAsked) context.startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName))
                            else { notificationAsked = true; requestNotificationThen(PendingNotificationAction.NOTIFICATIONS_ONLY) }
                        }
                    },
                    typingShare = typingShare,
                    typingWriteBusy = typingWriteBusy,
                    typingError = typingError,
                    onTypingShareChanged = { enabled ->
                        if (typingShare == null || busy || typingWriteBusy) return@StatusPanel
                        typingWriteBusy = true
                        typingError = false
                        scope.launch {
                            if (withContext(Dispatchers.IO) { host.setTypingSharing(enabled) }) typingShare = enabled
                            else typingError = true
                            typingWriteBusy = false
                        }
                    },
                )
            }
        }
    }
    when (sheet) {
        "consent" -> ConsentSheet(stringResource(R.string.peppy_google_messages_link_consent_title), listOf(
            stringResource(R.string.peppy_google_messages_link_consent_p_signin), stringResource(R.string.peppy_google_messages_link_consent_p_storage), stringResource(R.string.peppy_google_messages_link_consent_p_account), stringResource(R.string.peppy_google_messages_link_consent_p_emoji), stringResource(R.string.peppy_google_messages_link_consent_p_browser), stringResource(R.string.peppy_google_messages_link_consent_p_notification), stringResource(R.string.peppy_google_messages_link_consent_p_scope), stringResource(R.string.peppy_google_messages_link_consent_p_remove),
        ).joinToString("\n\n"), "google-messages-link-consent-sheet", { sheet = null }, { sheet = null; requestNotificationsThenSignIn() }, stringResource(R.string.peppy_google_messages_link_consent_confirm), showPrivacyPolicy = false)
        "relink" -> ConsentSheet(stringResource(R.string.peppy_google_messages_link_relink_title), stringResource(R.string.peppy_google_messages_link_relink_body) + "\n\n" + stringResource(R.string.peppy_google_messages_link_replaces_browser), "google-messages-link-relink-sheet", { sheet = null }, {
            sheet = null
            requestNotificationThen(PendingNotificationAction.RELINK)
        }, stringResource(R.string.peppy_google_messages_link_relink_confirm), showPrivacyPolicy = false)
        "unlink" -> Confirmation("unlink", R.string.peppy_google_messages_link_unlink_title, R.string.peppy_google_messages_link_unlink_body, R.string.peppy_google_messages_link_unlink_confirm, { sheet = null }) {
            val handle = (result as? LinkOpenResult.Open)?.handle ?: return@Confirmation
            scope.launch(start = CoroutineStart.UNDISPATCHED) {
                if (!LinkOperationLock.tryAcquire()) return@launch
                var persistenceFailed = false
                try {
                    refreshGeneration++
                    refreshJob?.cancel()
                    busy = true
                    host.beginOwnerUnlink()
                    withContext(Dispatchers.IO) { handle.unlink() }
                }
                catch (error: kotlinx.coroutines.CancellationException) { throw error }
                catch (error: Exception) {
                    persistenceFailed = error.message == "persist_failed"
                    // The reloaded status below determines the owner-visible outcome.
                } finally {
                    val reloadedStatus = withContext(NonCancellable) {
                        runCatching { reloadAuthoritativeStatus() }.getOrNull()
                    }
                    if (reloadedStatus?.state == "unlinked") {
                        val unlinkedRecorded = withContext(NonCancellable + Dispatchers.IO) { host.recordUnlinked() }
                        persistenceFailed = persistenceFailed || !unlinkedRecorded
                        if (unlinkedRecorded) {
                            val completed = withContext(NonCancellable + Dispatchers.IO) {
                                host.recordExplicitUnlinkCompleted()
                            }
                            persistenceFailed = persistenceFailed || !completed
                        }
                    }
                    localNotice = when {
                        persistenceFailed -> "persist_failed"
                        reloadedStatus?.state == "unlinked" && reloadedStatus.stopCode.isEmpty() -> "unlinked_done"
                        reloadedStatus?.state == "unlinked" && reloadedStatus.stopCode == "unlink_remote_failed" -> "unlink_remote_failed"
                        else -> null
                    }
                    withContext(NonCancellable) { LinkOperationLock.release() }
                    busy = false
                    refreshAfterAction()
                }
            }
        }
        "remove", "reset" -> Confirmation(sheet ?: "reset", if (sheet == "remove") R.string.peppy_google_messages_link_remove_title else R.string.peppy_google_messages_link_reset_title, if (sheet == "remove") R.string.peppy_google_messages_link_remove_body else R.string.peppy_google_messages_link_reset_body, if (sheet == "remove") R.string.peppy_google_messages_link_remove_confirm else R.string.peppy_google_messages_link_reset_confirm, { sheet = null }) {
            scope.launch(start = CoroutineStart.UNDISPATCHED) {
                if (!LinkOperationLock.tryAcquire()) return@launch
                var completed = false
                try {
                    refreshGeneration++
                    refreshJob?.cancel()
                    busy = true
                    withContext(Dispatchers.IO) { host.resetLocalState() }
                    completed = true
                }
                catch (error: kotlinx.coroutines.CancellationException) { throw error }
                catch (_: Exception) { localNotice = "persist_failed" }
                finally {
                    if (completed) localNotice = "removed"
                    withContext(NonCancellable) { LinkOperationLock.release() }
                    busy = false
                    refreshAfterAction()
                }
            }
        }
        "battery" -> ConsentSheet(stringResource(R.string.peppy_google_messages_link_battery_title), stringResource(R.string.peppy_google_messages_link_battery_sheet_body), "google-messages-link-battery-sheet", { sheet = null }, { sheet = null; openBatterySettings() }, stringResource(R.string.peppy_toggle_android_open_settings), showPrivacyPolicy = false)
    }
}

private class AndroidGoogleMessagesLinkPanelHost(
    private val context: android.content.Context,
    private val changingConfigurations: () -> Boolean,
    override val permissionResults: StateFlow<Long>,
    private val requestPermission: () -> Unit,
    private val launchSignIn: ((String?) -> Unit) -> Unit,
) : GoogleMessagesLinkPanelHost {
    override val changes = LinkRuntime.changes
    override val emoji = LinkRuntime.emoji
    override val notificationsGranted: Boolean
        get() = Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
    override val isChangingConfigurations: Boolean
        get() = changingConfigurations()

    override fun open() = LinkRuntime.handle(context.applicationContext)
    override fun recordPreStart() = LinkRuntime.recordPreStart(context.applicationContext)
    override fun recordPreStart(token: Long) = LinkRuntime.recordPreStart(context.applicationContext, token)
    override fun acquirePairingLease(token: Long) = LinkRuntime.acquirePairingLease(context.applicationContext, token)
    override fun captureStatusObservation(handle: dev.peppy.mobile.googlemessages.LinkHandle) =
        LinkRuntime.capturePanelObservation(handle)
    override fun recordStatusObservation(observation: LinkObservation, status: LinkStatus) =
        LinkRuntime.recordPanelObservation(context.applicationContext, observation, status)
    override fun beginOwnerUnlink() = LinkCoreBridge.invalidateObservations()
    override fun recordUnlinked() = LinkRuntime.recordUnlinked(context.applicationContext)
    override fun recordExplicitUnlinkCompleted() = LinkCoreBridge.recordExplicitUnlinkCompleted(context.applicationContext)
    override fun resetLocalState() = LinkRuntime.resetLocalState(context.applicationContext)
    override fun pairingInProgress() = LinkRuntime.pairingInProgress.get()
    override fun tryBeginPairing() = LinkRuntime.tryBeginPairing()
    override fun endPairing(token: Long) { LinkRuntime.endPairing(token) }
    override fun requestNotificationPermission() = requestPermission()
    override fun launchSignIn(onResult: (String?) -> Unit) = launchSignIn.invoke(onResult)
    override fun startService() = GoogleMessagesLinkService.start(context)
    private val typingSharingPreferences by lazy { TypingSharingPreferences(context) }

    override fun typingSharing() = typingSharingPreferences.enabled()
    override fun setTypingSharing(enabled: Boolean): Boolean {
        val client = NativeGateway.open(context) ?: return false
        return typingSharingPreferences.update(enabled, client::setTypingSharing)
    }
}

@Composable private fun Loading() = Column(Modifier.testTag("google-messages-link-loading")) { CircularProgressIndicator(Modifier.clearAndSetSemantics { }); Text(stringResource(R.string.peppy_google_messages_link_loading), Modifier.semantics { liveRegion = LiveRegionMode.Polite }) }

@Composable private fun LinkStatusRow(tag: String, label: String, value: String) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 8.dp).semantics(mergeDescendants = true) {
            liveRegion = LiveRegionMode.Polite
        }.testTag(tag),
    ) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
        Text(value, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable private fun UnreadablePanel(
    onReset: () -> Unit,
    busy: Boolean,
    localNotice: String?,
    onDismissNotice: () -> Unit,
) {
    if (localNotice != null) {
        Notice(localNotice, onDismissNotice)
    }
    LinkStatusRow("google-messages-link-status", stringResource(R.string.peppy_google_messages_link_status_label), stringResource(R.string.peppy_google_messages_link_status_unreadable))
    Text(stringResource(R.string.peppy_google_messages_link_unreadable_body), Modifier.testTag("google-messages-link-unreadable"))
    PanelButton(stringResource(R.string.peppy_google_messages_link_action_reset), !busy, "google-messages-link-reset", onReset)
}

@Composable private fun StatusPanel(status: LinkStatus, emoji: String?, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, signInBlocked: Boolean, pairingInProgress: Boolean, localNotice: String?, dismissedSeq: Long?, onDismissNotice: () -> Unit, onStart: () -> Unit, onRelink: () -> Unit, onUnlink: () -> Unit, onRemove: () -> Unit, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit, typingShare: Boolean?, typingWriteBusy: Boolean, typingError: Boolean, onTypingShareChanged: (Boolean) -> Unit) {
    val statusText = when (status.state) {
        "unlinked" -> R.string.peppy_google_messages_link_status_unlinked
        "pairing" -> R.string.peppy_google_messages_link_status_pairing
        "linked" -> R.string.peppy_google_messages_link_status_linked
        "needs_relink" -> R.string.peppy_google_messages_link_status_needs_relink
        "signed_out" -> R.string.peppy_google_messages_link_status_signed_out
        else -> R.string.peppy_google_messages_link_load_failed
    }
    if (localNotice != null || (status.stopCode.isNotEmpty() && dismissedSeq != status.seq)) {
        Notice(localNotice ?: status.stopCode, onDismissNotice)
    }
    LinkStatusRow("google-messages-link-status", stringResource(R.string.peppy_google_messages_link_status_label), stringResource(statusText))
    if (busy) {
        Text(stringResource(R.string.peppy_google_messages_link_working), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
    }
    when (status.state) {
        "unlinked" -> {
            Text(stringResource(R.string.peppy_google_messages_link_unlinked_body), Modifier.testTag("google-messages-link-unlinked"))
            Text(stringResource(R.string.peppy_google_messages_link_replaces_browser))
            if (signInBlocked) {
                Text(stringResource(R.string.peppy_google_messages_link_start_blocked))
            } else {
                PanelButton(stringResource(R.string.peppy_google_messages_link_action_link), !busy && !pairingInProgress, "google-messages-link-start", onStart)
            }
        }
        "pairing" -> Pairing(emoji, notificationsOff, busy, onAllowNotifications)
        "linked" -> Linked(status, busy, checkResult, notificationsOff, batteryAllowed, batteryUnavailable, onCheck, onReconnect, onBattery, onAllowNotifications, onUnlink, onRemove, typingShare, typingWriteBusy, typingError, onTypingShareChanged)
        "needs_relink" -> {
            Text(stringResource(R.string.peppy_google_messages_link_needs_relink_body), Modifier.testTag("google-messages-link-needs-relink"))
            PanelButton(stringResource(R.string.peppy_google_messages_link_action_relink), !busy && !pairingInProgress, "google-messages-link-relink", onRelink)
            Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy, "google-messages-link-unlink", onUnlink)
            TextButton(onClick = onRemove, enabled = !busy, modifier = Modifier.testTag("google-messages-link-remove-local")) {
                Text(stringResource(R.string.peppy_google_messages_link_action_remove_local))
            }
        }
        "signed_out" -> {
            Text(stringResource(R.string.peppy_google_messages_link_signed_out_body), Modifier.testTag("google-messages-link-signed-out"))
            PanelButton(stringResource(R.string.peppy_google_messages_link_action_sign_in_again), !busy && !pairingInProgress, "google-messages-link-sign-in-again", onStart)
            TextButton(onClick = onRemove, enabled = !busy, modifier = Modifier.testTag("google-messages-link-remove-local")) {
                Text(stringResource(R.string.peppy_google_messages_link_action_remove_local))
            }
        }
    }
}

@Composable private fun Pairing(emoji: String?, notificationsOff: Boolean, busy: Boolean, onAllowNotifications: () -> Unit) { if (emoji == null) Text(stringResource(R.string.peppy_google_messages_link_pairing_connecting), Modifier.testTag("google-messages-link-pairing").semantics { liveRegion = LiveRegionMode.Polite }) else Column(Modifier.testTag("google-messages-link-emoji-card").semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Assertive }) { Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_instruction)); Text(emoji, Modifier.testTag("google-messages-link-emoji"), style = MaterialTheme.typography.displayLarge); Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_hint)) }; if (notificationsOff) Notifications(busy = busy, onAllow = onAllowNotifications) }

@Composable private fun Linked(status: LinkStatus, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit, onUnlink: () -> Unit, onRemove: () -> Unit, typingShare: Boolean?, typingWriteBusy: Boolean, typingError: Boolean, onTypingShareChanged: (Boolean) -> Unit) {
    Text(stringResource(R.string.peppy_google_messages_link_linked_body), Modifier.testTag("google-messages-link-linked"))
    val connection = when {
        !status.running -> R.string.peppy_google_messages_link_connection_not_running
        !status.phoneResponding -> R.string.peppy_google_messages_link_connection_phone_not_responding
        !status.healthy -> R.string.peppy_google_messages_link_connection_unconfirmed
        else -> R.string.peppy_google_messages_link_connection_connected
    }
    LinkStatusRow("google-messages-link-connection", stringResource(R.string.peppy_google_messages_link_connection_label), stringResource(connection))
    LinkStatusRow("google-messages-link-last-ok", stringResource(R.string.peppy_google_messages_link_last_ok_label), if (status.lastOkMs == 0L) stringResource(R.string.peppy_google_messages_link_last_ok_never) else DateUtils.getRelativeTimeSpanString(status.lastOkMs).toString())
    if (!status.running) {
        Text(stringResource(R.string.peppy_google_messages_link_help_not_running))
        if (status.stopCode != "persist_failed") {
            TextButton(onClick = onReconnect, enabled = !busy, modifier = Modifier.testTag("google-messages-link-reconnect")) {
                Text(stringResource(R.string.peppy_google_messages_link_action_reconnect))
            }
        }
    } else {
        if (!status.phoneResponding) {
            Text(stringResource(R.string.peppy_google_messages_link_help_phone_not_responding))
        } else if (!status.healthy) {
            Text(stringResource(R.string.peppy_google_messages_link_help_unconfirmed))
        }
        PanelButton(if (busy) stringResource(R.string.peppy_google_messages_link_checking) else stringResource(R.string.peppy_google_messages_link_action_check_now), !busy, "google-messages-link-check-now", onCheck)
    }
    checkResult?.let {
        Text(stringResource(if (it >= 0) R.string.peppy_google_messages_link_check_ok else R.string.peppy_google_messages_link_check_failed), Modifier.testTag("google-messages-link-check-result").semantics { liveRegion = LiveRegionMode.Polite })
    }
    SettingToggleRow(stringResource(R.string.peppy_google_messages_link_battery_title), batteryAllowed, !busy, { onBattery() }, "google-messages-link-battery-row", stringResource(R.string.peppy_google_messages_link_battery_subtitle), stringResource(R.string.peppy_toggle_android_open_settings), stringResource(if (batteryAllowed) R.string.peppy_toggle_android_battery_allowed else R.string.peppy_toggle_android_battery_delayed))
    if (batteryUnavailable) {
        Text(stringResource(R.string.peppy_toggle_android_battery_unavailable), Modifier.testTag("google-messages-link-battery-unavailable"))
    }
    Row(Modifier.fillMaxWidth()) {
        Box(Modifier.weight(1f)) {
            SettingToggleRow(
                title = stringResource(R.string.peppy_typing_share_label),
                checked = typingShare ?: true,
                enabled = typingShare != null && !busy && !typingWriteBusy,
                onCheckedChange = onTypingShareChanged,
                tag = "google-messages-link-typing-row",
            )
        }
        InfoButton(
            topic = stringResource(R.string.peppy_typing_share_label),
            title = stringResource(R.string.peppy_typing_share_label),
            body = stringResource(R.string.peppy_typing_share_hint),
            tag = "info-typing-share",
        )
    }
    if (typingError) Text(stringResource(R.string.peppy_setting_update_error), Modifier.testTag("google-messages-link-typing-error").semantics { liveRegion = LiveRegionMode.Assertive })
    if (notificationsOff) {
        Notifications(busy, onAllowNotifications)
    }
    Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy, "google-messages-link-unlink", onUnlink)
    if (status.stopCode == "persist_failed") {
        TextButton(onClick = onRemove, enabled = !busy, modifier = Modifier.testTag("google-messages-link-remove-local")) {
            Text(stringResource(R.string.peppy_google_messages_link_action_remove_local))
        }
    }
}

@Composable private fun Notifications(busy: Boolean, onAllow: () -> Unit) = Column(Modifier.testTag("google-messages-link-notifications-row")) { Text(stringResource(R.string.peppy_google_messages_link_notifications_off)); TextButton(onClick = onAllow, enabled = !busy, modifier = Modifier.testTag("google-messages-link-notifications-allow")) { Text(stringResource(R.string.peppy_remote_messaging_allow_notifications)) } }
@Composable private fun Notice(code: String, dismiss: () -> Unit) {
    val text = when (code) {
        "removed" -> R.string.peppy_google_messages_link_notice_removed_local
        "unlinked_done" -> R.string.peppy_google_messages_link_unlinked_done
        "unlink_remote_failed" -> R.string.peppy_google_messages_link_notice_unlink_remote_failed
        "pair_no_phone" -> R.string.peppy_google_messages_link_notice_pair_no_phone
        "pair_multiple_phones" -> R.string.peppy_google_messages_link_notice_pair_multiple_phones
        "pair_wrong_emoji" -> R.string.peppy_google_messages_link_notice_pair_wrong_emoji
        "pair_cancelled" -> R.string.peppy_google_messages_link_notice_pair_cancelled
        "pair_timeout" -> R.string.peppy_google_messages_link_notice_pair_timeout
        "account_mismatch" -> R.string.peppy_google_messages_link_notice_account_mismatch
        "pair_failed" -> R.string.peppy_google_messages_link_notice_pair_failed
        "relink_failed" -> R.string.peppy_google_messages_link_notice_relink_failed
        "persist_failed" -> R.string.peppy_google_messages_link_notice_persist_failed
        "signin_blocked" -> R.string.peppy_google_messages_link_notice_signin_blocked
        "signin_incomplete" -> R.string.peppy_google_messages_link_notice_signin_incomplete
        else -> R.string.peppy_google_messages_link_notice_unknown
    }
    val polite = code in setOf("removed", "unlinked_done", "pair_cancelled")
    Column(Modifier.testTag("google-messages-link-notice").semantics { liveRegion = if (polite) LiveRegionMode.Polite else LiveRegionMode.Assertive }) { Text(stringResource(text)); TextButton(onClick = dismiss, modifier = Modifier.testTag("google-messages-link-notice-dismiss")) { Text(stringResource(R.string.peppy_google_messages_link_notice_dismiss)) } }
}
@Composable private fun PanelButton(label: String, enabled: Boolean, tag: String, onClick: () -> Unit) = Button(onClick = onClick, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag)) { Text(label) }
@Composable private fun Secondary(label: String, enabled: Boolean, tag: String, onClick: () -> Unit) = OutlinedButton(onClick = onClick, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag)) { Text(label) }
@Composable private fun Confirmation(kind: String, title: Int, body: Int, action: Int, onClose: () -> Unit, onConfirm: () -> Unit) = ConsentSheet(stringResource(title), stringResource(body), "google-messages-link-$kind-sheet", onClose, { onClose(); onConfirm() }, stringResource(action), nextStep = stringResource(R.string.peppy_google_messages_link_manual_removal_steps), showPrivacyPolicy = false)
