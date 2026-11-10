package dev.peppy.mobile.googlemessages.ui

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.text.format.DateUtils
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
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
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.googlemessages.GoogleMessagesLinkService
import dev.peppy.mobile.googlemessages.LinkCoreBridge
import dev.peppy.mobile.googlemessages.LinkObservation
import dev.peppy.mobile.googlemessages.LinkOpenResult
import dev.peppy.mobile.googlemessages.LinkOperationLock
import dev.peppy.mobile.googlemessages.LinkRuntime
import dev.peppy.mobile.googlemessages.LinkSignInActivity
import dev.peppy.mobile.googlemessages.LinkStatus
import dev.peppy.mobile.googlemessages.TypingSharingPreferences
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.DetailsPageHeader
import dev.peppy.mobile.ui.DetailsStatusRow
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.theme.PeppyTokens
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

// Card and details keep UI memory that SmsTab hoists, so a Manage round trip does not reset it.
@Composable
internal fun GoogleMessagesLinkPanel(onChanged: () -> Unit) =
    GoogleMessagesLinkPanelContent(null, onChanged, null, null, {}, null, {}, false, {}, details = false, onBack = {})

@Composable
internal fun GoogleMessagesLinkPanel(
    onChanged: () -> Unit,
    notice: String?,
    onNoticeChange: (String?) -> Unit,
    onManage: () -> Unit,
    dismissedSeq: Long?,
    onDismissedSeqChange: (Long?) -> Unit,
    notificationAsked: Boolean,
    onNotificationAskedChange: (Boolean) -> Unit,
) = GoogleMessagesLinkPanelContent(
    null, onChanged, onManage, notice, onNoticeChange, dismissedSeq, onDismissedSeqChange,
    notificationAsked, onNotificationAskedChange, details = false, onBack = {},
)

@Composable
internal fun GoogleMessagesLinkPanel(
    injectedHost: GoogleMessagesLinkPanelHost,
    onChanged: () -> Unit,
    onManage: () -> Unit = {},
    notice: String? = null,
    onNoticeChange: (String?) -> Unit = {},
    dismissedSeq: Long? = null,
    onDismissedSeqChange: (Long?) -> Unit = {},
    notificationAsked: Boolean = false,
    onNotificationAskedChange: (Boolean) -> Unit = {},
) = GoogleMessagesLinkPanelContent(
    injectedHost, onChanged, onManage, notice, onNoticeChange, dismissedSeq, onDismissedSeqChange,
    notificationAsked, onNotificationAskedChange, details = false, onBack = {},
)

@Composable
internal fun GoogleMessagesLinkPanelDetails(
    onBack: () -> Unit,
    onChanged: () -> Unit,
    notice: String?,
    onNoticeChange: (String?) -> Unit,
    dismissedSeq: Long?,
    onDismissedSeqChange: (Long?) -> Unit,
    notificationAsked: Boolean,
    onNotificationAskedChange: (Boolean) -> Unit,
) = GoogleMessagesLinkPanelContent(
    null, onChanged, null, notice, onNoticeChange, dismissedSeq, onDismissedSeqChange,
    notificationAsked, onNotificationAskedChange, details = true, onBack = onBack,
)

@Composable
internal fun GoogleMessagesLinkPanelDetails(
    injectedHost: GoogleMessagesLinkPanelHost,
    onBack: () -> Unit = {},
    onChanged: () -> Unit,
    notice: String? = null,
    onNoticeChange: (String?) -> Unit = {},
    dismissedSeq: Long? = null,
    onDismissedSeqChange: (Long?) -> Unit = {},
    notificationAsked: Boolean = false,
    onNotificationAskedChange: (Boolean) -> Unit = {},
) = GoogleMessagesLinkPanelContent(
    injectedHost, onChanged, null, notice, onNoticeChange, dismissedSeq, onDismissedSeqChange,
    notificationAsked, onNotificationAskedChange, details = true, onBack = onBack,
)

@Composable
private fun GoogleMessagesLinkPanelContent(
    injectedHost: GoogleMessagesLinkPanelHost?,
    onChanged: () -> Unit,
    onManage: (() -> Unit)?,
    initialNotice: String?,
    onNoticeChange: (String?) -> Unit,
    dismissedSeq: Long?,
    onDismissedSeqChange: (Long?) -> Unit,
    notificationAsked: Boolean,
    onNotificationAskedChange: (Boolean) -> Unit,
    details: Boolean,
    onBack: () -> Unit,
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
    var localNotice by rememberSaveable(initialNotice) { mutableStateOf(initialNotice) }
    var cutoverAlert by remember { mutableStateOf(false) }
    var cutoverDismissed by rememberSaveable { mutableStateOf(false) }
    var batteryUnavailable by remember { mutableStateOf(false) }
    var batteryRefresh by remember { mutableIntStateOf(0) }
    var typingShare by remember { mutableStateOf<Boolean?>(null) }
    var typingWriteBusy by remember { mutableStateOf(false) }
    var typingError by remember { mutableStateOf(false) }
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

    // Every notice write also reports to the parent, which owns it across card and details.
    fun setNotice(value: String?) {
        localNotice = value
        onNoticeChange(value)
    }
    fun detailsGone(): Boolean = result is LinkOpenResult.Unreadable || status?.state in setOf("unlinked", "unknown")

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
    LaunchedEffect(changes, resume, status?.state) {
        if (!details) {
            val alert = withContext(Dispatchers.IO) { readCutoverAlert(context) }
            cutoverAlert = alert
            if (alert) cutoverDismissed = false
        }
    }
    // Suppressed while an operation runs: the operation's own finally block decides when details closes.
    LaunchedEffect(details, status?.state, result) {
        if (details && !busy && !destructiveOperationInProgress && detailsGone()) onBack()
    }

    fun refreshAfterAction() { resume++; onChanged() }
    fun launchSignIn() {
        host.launchSignIn { outcome ->
            setNotice(
                when (outcome) {
                    "blocked" -> "signin_blocked"
                    "cookies_missing", "error" -> "signin_incomplete"
                    else -> null
                },
            )
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
                    setNotice("persist_failed")
                    return@launch
                }
                host.startService()
                withContext(Dispatchers.IO) { lease.handle.relink() }
            }
            catch (error: kotlinx.coroutines.CancellationException) { throw error }
            catch (_: Exception) { setNotice("relink_failed") }
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

    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    val unreadable = result is LinkOpenResult.Unreadable
    val signInBlocked = GoogleMessagesLink.signInBlocked(context)
    val stopNoticeShown = status?.let { it.stopCode.isNotEmpty() && dismissedSeq != it.seq } == true && !details
    val noticeCode = localNotice ?: status?.stopCode?.takeIf { stopNoticeShown }
    val assertiveNotice = noticeCode != null && noticeCode !in setOf("removed", "unlinked_done", "pair_cancelled")
    // Running and phone-responding only describe a linked link; unlinked and pairing report them as false.
    val degradedLink = status?.let { it.state == "linked" && (!it.running || !it.phoneResponding) } == true
    val warning = unreadable || assertiveNotice || signInBlocked || degradedLink ||
        status?.state in setOf("needs_relink", "signed_out")
    val showCutover = cutoverAlert && !cutoverDismissed && status != null && !loadingFailed && !unreadable
    val content: @Composable () -> Unit = {
        when {
            result == null && !loadingFailed -> Loading()
            loadingFailed -> {
                Text(stringResource(R.string.peppy_google_messages_link_load_failed), Modifier.semantics { liveRegion = LiveRegionMode.Assertive }.testTag("google-messages-link-load-failed"))
                TextButton(onClick = ::reload, modifier = Modifier.testTag("google-messages-link-reload")) { Text(stringResource(R.string.peppy_try_again)) }
            }
            unreadable -> UnreadablePanel(
                onReset = { sheet = "reset" },
                busy = busy || destructiveOperationInProgress,
                localNotice = localNotice,
                onDismissNotice = { setNotice(null) },
            )
            status != null -> StatusPanel(
                status = checkNotNull(status), emoji = emoji, busy = busy || destructiveOperationInProgress, checkResult = checkResult,
                notificationsOff = notificationsOff, batteryAllowed = batteryAllowed, batteryUnavailable = batteryUnavailable,
                signInBlocked = signInBlocked, pairingInProgress = host.pairingInProgress(), localNotice = localNotice,
                // Only a stop-code notice is recorded as dismissed; dismissing a local notice leaves the stop code visible.
                dismissedSeq = dismissedSeq, onDismissNotice = {
                    if (localNotice == null) onDismissedSeqChange(status?.seq)
                    setNotice(null)
                },
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
                        else { onNotificationAskedChange(true); requestNotificationThen(PendingNotificationAction.NOTIFICATIONS_ONLY) }
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
                }, details = details, onManage = onManage,
            )
        }
    }
    if (details) {
        ScreenBody("google-messages-details-screen") {
            DetailsPageHeader(stringResource(R.string.peppy_google_messages_link_details_title), "google-messages-details", onBack)
            content()
        }
    } else {
        Surface(
            modifier = Modifier.fillMaxWidth().testTag("google-messages-link-card"),
            shape = RoundedCornerShape(12.dp),
            color = if (warning) colors.warningSurface else colors.surfaceEditor,
            border = BorderStroke(1.dp, if (warning) colors.warningBorder else colors.borderStrong),
        ) {
            Column(
                Modifier.padding(16.dp).testTag(if (warning) "google-messages-link-card-tone-warning" else "google-messages-link-card-tone-neutral"),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                LinkCardHeader(status, unreadable, warningCard = warning)
                if (showCutover) {
                    Column(Modifier.testTag("google-messages-link-notice-cutover").semantics { liveRegion = LiveRegionMode.Polite }) {
                        Text(stringResource(R.string.peppy_google_messages_link_notice_cutover_ambiguous))
                        TextButton(
                            onClick = {
                                scope.launch {
                                    val alert = withContext(Dispatchers.IO) {
                                        dismissCutoverAlert(context)
                                        readCutoverAlert(context)
                                    }
                                    cutoverDismissed = true
                                    cutoverAlert = alert
                                }
                            },
                            modifier = Modifier.testTag("google-messages-link-notice-cutover-dismiss"),
                        ) { Text(stringResource(R.string.peppy_google_messages_link_notice_dismiss)) }
                    }
                }
                content()
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
                }
                finally {
                    withContext(NonCancellable) {
                        val reloadedStatus = runCatching { reloadAuthoritativeStatus() }.getOrNull()
                        if (reloadedStatus?.state == "unlinked") {
                            val unlinkedRecorded = withContext(Dispatchers.IO) { host.recordUnlinked() }
                            persistenceFailed = persistenceFailed || !unlinkedRecorded
                            if (unlinkedRecorded) {
                                val completed = withContext(Dispatchers.IO) { host.recordExplicitUnlinkCompleted() }
                                persistenceFailed = persistenceFailed || !completed
                            }
                        }
                        setNotice(
                            when {
                                persistenceFailed -> "persist_failed"
                                reloadedStatus?.state == "unlinked" && reloadedStatus.stopCode.isEmpty() -> "unlinked_done"
                                reloadedStatus?.state == "unlinked" && reloadedStatus.stopCode == "unlink_remote_failed" -> "unlink_remote_failed"
                                reloadedStatus?.stopCode == "persist_failed" -> "persist_failed"
                                else -> "unlink_failed"
                            },
                        )
                        LinkOperationLock.release()
                        busy = false
                        refreshAfterAction()
                        if (details && detailsGone()) onBack()
                    }
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
                catch (_: Exception) { setNotice("persist_failed") }
                finally {
                    withContext(NonCancellable) {
                        if (completed) setNotice("removed")
                        LinkOperationLock.release()
                        busy = false
                        refreshAfterAction()
                        if (details) {
                            runCatching { reloadAuthoritativeStatus() }
                            if (detailsGone()) onBack()
                        }
                    }
                }
            }
        }
        "battery" -> ConsentSheet(stringResource(R.string.peppy_google_messages_link_battery_title), stringResource(R.string.peppy_google_messages_link_battery_sheet_body), "google-messages-link-battery-sheet", { sheet = null }, { sheet = null; openBatterySettings() }, stringResource(R.string.peppy_toggle_android_open_settings), showPrivacyPolicy = false)
    }
}

/** Reads the cutover alert. Isolated so a core-backed read can replace the prefs flag in one place. */
private fun readCutoverAlert(context: Context): Boolean = GoogleMessagesLink.cutoverMayHaveMissedMessages(context)

/** Clears the cutover alert after the user dismisses it, so it does not return. */
private fun dismissCutoverAlert(context: Context) = GoogleMessagesLink.clearCutoverMayHaveMissedMessages(context)

@Composable
private fun LinkCardHeader(status: LinkStatus?, unreadable: Boolean, warningCard: Boolean) {
    val fontScale = LocalDensity.current.fontScale
    val title = stringResource(R.string.peppy_google_messages_link_card_title)
    val showChip = status != null || unreadable
    val info: @Composable () -> Unit = {
        InfoButton(title, stringResource(R.string.peppy_google_messages_link_help_title), stringResource(R.string.peppy_google_messages_link_help_body), "info-google-messages-link")
    }
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        if (maxWidth < 360.dp || fontScale > 1.15f) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(title, Modifier.weight(1f).semantics { heading() }.testTag("google-messages-link-heading"), style = MaterialTheme.typography.titleMedium)
                    info()
                }
                if (showChip) LinkChip(status, unreadable, warningCard)
            }
        } else {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(title, Modifier.weight(1f).semantics { heading() }.testTag("google-messages-link-heading"), style = MaterialTheme.typography.titleMedium)
                if (showChip) LinkChip(status, unreadable, warningCard)
                info()
            }
        }
    }
}

private enum class ChipTone { Success, Warning, Neutral }

@Composable private fun LinkChip(status: LinkStatus?, unreadable: Boolean, warningCard: Boolean) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    val label = when {
        unreadable -> R.string.peppy_google_messages_link_status_unreadable
        status?.state == "unlinked" -> R.string.peppy_google_messages_link_status_unlinked
        status?.state == "pairing" -> R.string.peppy_google_messages_link_status_pairing
        status?.state == "linked" -> when {
            !status.running -> R.string.peppy_google_messages_link_connection_not_running
            !status.phoneResponding -> R.string.peppy_google_messages_link_connection_phone_not_responding
            !status.healthy -> R.string.peppy_google_messages_link_connection_unconfirmed
            else -> R.string.peppy_google_messages_link_status_linked
        }
        status?.state == "needs_relink" -> R.string.peppy_google_messages_link_status_needs_relink
        status?.state == "signed_out" -> R.string.peppy_google_messages_link_status_signed_out
        else -> R.string.peppy_google_messages_link_status_unreadable
    }
    val tone = when {
        unreadable -> ChipTone.Warning
        status == null -> ChipTone.Neutral
        status.state == "linked" && (!status.running || !status.phoneResponding) -> ChipTone.Warning
        status.state == "linked" && status.healthy -> ChipTone.Success
        status.state == "linked" || status.state == "unlinked" || status.state == "pairing" -> ChipTone.Neutral
        else -> ChipTone.Warning
    }
    // On a warning card the warning fill would vanish into the same colour, so the chip uses the card surface there.
    val fill = when (tone) {
        ChipTone.Success -> colors.success.copy(alpha = 0.12f)
        ChipTone.Warning -> if (warningCard) colors.surfaceEditor else colors.warningSurface
        ChipTone.Neutral -> colors.surfacePanel
    }
    val textColor = when (tone) {
        ChipTone.Success -> colors.textPrimary
        ChipTone.Warning -> colors.warningText
        ChipTone.Neutral -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    val glyph = when (tone) {
        ChipTone.Success -> "✓"
        ChipTone.Warning -> "⚠"
        ChipTone.Neutral -> null
    }
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = fill,
        border = when (tone) {
            ChipTone.Success -> null
            ChipTone.Warning -> BorderStroke(1.dp, colors.warningBorder)
            ChipTone.Neutral -> BorderStroke(1.dp, colors.border)
        },
        modifier = Modifier.testTag("google-messages-link-chip").semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
    ) {
        Row(Modifier.padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            glyph?.let {
                Text(
                    it,
                    Modifier.clearAndSetSemantics { },
                    color = if (tone == ChipTone.Success) colors.success else colors.warningText,
                    style = MaterialTheme.typography.labelMedium,
                )
                Spacer(Modifier.width(4.dp))
            }
            Text(stringResource(label), color = textColor, style = MaterialTheme.typography.labelMedium)
        }
    }
}

private class AndroidGoogleMessagesLinkPanelHost(
    private val context: Context,
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

@Composable private fun UnreadablePanel(
    onReset: () -> Unit,
    busy: Boolean,
    localNotice: String?,
    onDismissNotice: () -> Unit,
) {
    if (localNotice != null) {
        Notice(localNotice, onDismissNotice)
    }
    Text(stringResource(R.string.peppy_google_messages_link_unreadable_body), Modifier.testTag("google-messages-link-unreadable"))
    PanelButton(stringResource(R.string.peppy_google_messages_link_action_reset), !busy, "google-messages-link-reset", onReset)
}

@Composable private fun StatusPanel(status: LinkStatus, emoji: String?, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, signInBlocked: Boolean, pairingInProgress: Boolean, localNotice: String?, dismissedSeq: Long?, onDismissNotice: () -> Unit, onStart: () -> Unit, onRelink: () -> Unit, onUnlink: () -> Unit, onRemove: () -> Unit, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit, typingShare: Boolean?, typingWriteBusy: Boolean, typingError: Boolean, onTypingShareChanged: (Boolean) -> Unit, details: Boolean, onManage: (() -> Unit)?) {
    val statusText = when (status.state) {
        "unlinked" -> R.string.peppy_google_messages_link_status_unlinked
        "pairing" -> R.string.peppy_google_messages_link_status_pairing
        "linked" -> R.string.peppy_google_messages_link_status_linked
        "needs_relink" -> R.string.peppy_google_messages_link_status_needs_relink
        "signed_out" -> R.string.peppy_google_messages_link_status_signed_out
        else -> R.string.peppy_google_messages_link_load_failed
    }
    // Stop codes are announced on the card only; details shows the notice the user just caused.
    val stopNoticeVisible = !details && status.stopCode.isNotEmpty() && dismissedSeq != status.seq
    if (localNotice != null || stopNoticeVisible) {
        Notice(localNotice ?: status.stopCode, onDismissNotice)
    }
    if (details) DetailsStatusRow("google-messages-link-status", stringResource(R.string.peppy_google_messages_link_status_label), stringResource(statusText))
    if (busy) {
        Text(stringResource(R.string.peppy_google_messages_link_working), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
    }
    when (status.state) {
        "unlinked" -> {
            Text(stringResource(R.string.peppy_google_messages_link_card_not_linked_body), Modifier.testTag("google-messages-link-unlinked"))
            if (signInBlocked) {
                Text(stringResource(R.string.peppy_google_messages_link_start_blocked))
            } else {
                PanelButton(stringResource(R.string.peppy_google_messages_link_action_link), !busy && !pairingInProgress, "google-messages-link-start", onStart)
            }
            Text(stringResource(R.string.peppy_google_messages_link_not_messaging_app), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        "pairing" -> Pairing(emoji, notificationsOff, busy, onAllowNotifications)
        "linked" -> Linked(status, busy, checkResult, notificationsOff, batteryAllowed, batteryUnavailable, onCheck, onReconnect, onBattery, onAllowNotifications, onUnlink, onRemove, typingShare, typingWriteBusy, typingError, onTypingShareChanged, details, pairingInProgress)
        "needs_relink" -> {
            Text(stringResource(R.string.peppy_google_messages_link_needs_relink_body), Modifier.testTag("google-messages-link-needs-relink"))
            PanelButton(stringResource(R.string.peppy_google_messages_link_action_relink), !busy && !pairingInProgress, "google-messages-link-relink", onRelink)
            if (details) {
                Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy && !pairingInProgress, "google-messages-link-unlink", onUnlink)
                TextButton(onClick = onRemove, enabled = !busy && !pairingInProgress, modifier = Modifier.testTag("google-messages-link-remove-local")) { Text(stringResource(R.string.peppy_google_messages_link_action_remove_local)) }
            }
        }
        "signed_out" -> {
            Text(stringResource(R.string.peppy_google_messages_link_signed_out_body), Modifier.testTag("google-messages-link-signed-out"))
            PanelButton(stringResource(R.string.peppy_google_messages_link_action_sign_in_again), !busy && !pairingInProgress, "google-messages-link-sign-in-again", onStart)
            if (details) TextButton(onClick = onRemove, enabled = !busy && !pairingInProgress, modifier = Modifier.testTag("google-messages-link-remove-local")) { Text(stringResource(R.string.peppy_google_messages_link_action_remove_local)) }
        }
    }
    if (!details && status.state in setOf("linked", "needs_relink", "signed_out")) {
        HorizontalDivider()
        // Manage is disabled while an operation or pairing could change the link underneath the details page.
        TextButton(
            onClick = { onManage?.invoke() },
            enabled = !busy && !pairingInProgress,
            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("google-messages-link-manage"),
        ) { Text(stringResource(R.string.peppy_google_messages_link_action_manage)) }
    }
}

@Composable private fun Pairing(emoji: String?, notificationsOff: Boolean, busy: Boolean, onAllowNotifications: () -> Unit) { if (emoji == null) Text(stringResource(R.string.peppy_google_messages_link_pairing_connecting), Modifier.testTag("google-messages-link-pairing").semantics { liveRegion = LiveRegionMode.Polite }) else Column(Modifier.testTag("google-messages-link-emoji-card").semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Assertive }) { Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_instruction)); Text(emoji, Modifier.testTag("google-messages-link-emoji"), style = MaterialTheme.typography.displayLarge); Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_hint)) }; if (notificationsOff) Notifications(busy = busy, onAllow = onAllowNotifications) }

@Composable private fun Linked(status: LinkStatus, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit, onUnlink: () -> Unit, onRemove: () -> Unit, typingShare: Boolean?, typingWriteBusy: Boolean, typingError: Boolean, onTypingShareChanged: (Boolean) -> Unit, details: Boolean, pairingInProgress: Boolean) {
    val connection = when {
        !status.running -> R.string.peppy_google_messages_link_connection_not_running
        !status.phoneResponding -> R.string.peppy_google_messages_link_connection_phone_not_responding
        !status.healthy -> R.string.peppy_google_messages_link_connection_unconfirmed
        else -> R.string.peppy_google_messages_link_connection_connected
    }
    Column(Modifier.testTag("google-messages-link-linked"), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        DetailsStatusRow("google-messages-link-connection", stringResource(R.string.peppy_google_messages_link_connection_label), stringResource(connection))
        DetailsStatusRow("google-messages-link-last-ok", stringResource(R.string.peppy_google_messages_link_last_ok_label), if (status.lastOkMs == 0L) stringResource(R.string.peppy_google_messages_link_last_ok_never) else DateUtils.getRelativeTimeSpanString(status.lastOkMs).toString())
        if (!status.running) {
            Text(stringResource(R.string.peppy_google_messages_link_help_not_running))
            if (status.stopCode != "persist_failed") {
                PanelButton(stringResource(R.string.peppy_google_messages_link_action_reconnect), !busy, "google-messages-link-reconnect", onReconnect)
            }
        } else {
            if (!status.phoneResponding) {
                Text(stringResource(R.string.peppy_google_messages_link_help_phone_not_responding))
            } else if (!status.healthy) {
                Text(stringResource(R.string.peppy_google_messages_link_help_unconfirmed))
            }
            // The card offers Check now only while the link is degraded; the details page always offers it.
            if (details || !status.healthy || !status.phoneResponding) {
                PanelButton(if (busy) stringResource(R.string.peppy_google_messages_link_checking) else stringResource(R.string.peppy_google_messages_link_action_check_now), !busy, "google-messages-link-check-now", onCheck)
            }
        }
        checkResult?.let {
            Text(stringResource(if (it >= 0) R.string.peppy_google_messages_link_check_ok else R.string.peppy_google_messages_link_check_failed), Modifier.testTag("google-messages-link-check-result").semantics { liveRegion = LiveRegionMode.Polite })
        }
        if (details) {
            HorizontalDivider()
            SettingToggleRow(stringResource(R.string.peppy_google_messages_link_battery_title), batteryAllowed, !busy, { onBattery() }, "google-messages-link-battery-row", stringResource(R.string.peppy_google_messages_link_battery_subtitle), stringResource(R.string.peppy_toggle_android_open_settings), stringResource(if (batteryAllowed) R.string.peppy_toggle_android_battery_allowed else R.string.peppy_toggle_android_battery_delayed))
            if (batteryUnavailable) {
                Text(stringResource(R.string.peppy_toggle_android_battery_unavailable), Modifier.testTag("google-messages-link-battery-unavailable"))
            }
            Row(Modifier.fillMaxWidth()) {
                Box(Modifier.weight(1f)) {
                    SettingToggleRow(
                        title = stringResource(R.string.peppy_typing_share_label),
                        // Unknown until the confirmed value loads: shown unchecked and disabled, never assumed on.
                        checked = typingShare ?: false,
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
            HorizontalDivider()
            Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy && !pairingInProgress, "google-messages-link-unlink", onUnlink)
            if (status.stopCode == "persist_failed") {
                TextButton(onClick = onRemove, enabled = !busy && !pairingInProgress, modifier = Modifier.testTag("google-messages-link-remove-local")) {
                    Text(stringResource(R.string.peppy_google_messages_link_action_remove_local))
                }
            }
        }
    }
}

@Composable private fun Notifications(busy: Boolean, onAllow: () -> Unit) = Column(Modifier.testTag("google-messages-link-notifications-row")) { Text(stringResource(R.string.peppy_google_messages_link_notifications_off)); TextButton(onClick = onAllow, enabled = !busy, modifier = Modifier.testTag("google-messages-link-notifications-allow")) { Text(stringResource(R.string.peppy_remote_messaging_allow_notifications)) } }
@Composable private fun Notice(code: String, dismiss: () -> Unit) {
    val text = when (code) {
        "removed" -> R.string.peppy_google_messages_link_notice_removed_local
        "unlinked_done" -> R.string.peppy_google_messages_link_unlinked_done
        "unlink_remote_failed" -> R.string.peppy_google_messages_link_notice_unlink_remote_failed
        "unlink_failed" -> R.string.peppy_google_messages_link_notice_unlink_failed
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
