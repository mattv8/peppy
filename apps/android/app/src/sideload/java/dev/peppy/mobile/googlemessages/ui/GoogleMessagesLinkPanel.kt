package dev.peppy.mobile.googlemessages.ui

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.text.format.DateUtils
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.R
import dev.peppy.mobile.StatusRow
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import dev.peppy.mobile.googlemessages.GoogleMessagesLinkService
import dev.peppy.mobile.googlemessages.LinkOpenResult
import dev.peppy.mobile.googlemessages.LinkRuntime
import dev.peppy.mobile.googlemessages.LinkSignInActivity
import dev.peppy.mobile.googlemessages.LinkStatus
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun GoogleMessagesLinkPanel(onChanged: () -> Unit) {
    val context = LocalContext.current
    if (!GoogleMessagesLink.available(context)) return
    val scope = rememberCoroutineScope()
    val changes by LinkRuntime.changes.collectAsState()
    val emoji by LinkRuntime.emoji.collectAsState()
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
    var notificationAsked by rememberSaveable { mutableStateOf(false) }

    fun reload() {
        scope.launch {
            loadingFailed = false
            try {
                val opened = withContext(Dispatchers.IO) { LinkRuntime.handle(context.applicationContext) }
                val loadedStatus = (opened as? LinkOpenResult.Open)?.handle?.let { handle ->
                    withContext(Dispatchers.IO) { runCatching { LinkStatus.parse(handle.status()) }.getOrNull() }
                }
                if (opened is LinkOpenResult.Open && (loadedStatus == null || loadedStatus.state == "unknown")) loadingFailed = true
                else { result = opened; status = loadedStatus }
            } catch (_: Exception) { loadingFailed = true }
        }
    }
    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                resume++
                batteryRefresh++
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(changes, resume) { reload() }

    val startSignIn = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { outcome ->
        localNotice = when (outcome.data?.getStringExtra("dev.peppy.mobile.googlemessages.outcome")) {
            "blocked" -> "signin_blocked"
            "cookies_missing", "error" -> "signin_incomplete"
            else -> null
        }
        resume++
    }
    val linkNotificationRequest = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        startSignIn.launch(Intent(context, LinkSignInActivity::class.java))
    }
    val notificationRequest = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    fun requestNotificationsThenSignIn() {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            linkNotificationRequest.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else startSignIn.launch(Intent(context, LinkSignInActivity::class.java))
    }
    fun refreshAfterAction() { resume++; onChanged() }
    val notificationsOff = Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
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
                result is LinkOpenResult.Unreadable -> UnreadablePanel({ sheet = "reset" }, busy)
                status != null -> StatusPanel(
                    status = checkNotNull(status), emoji = emoji, busy = busy, checkResult = checkResult,
                    notificationsOff = notificationsOff, batteryAllowed = batteryAllowed, batteryUnavailable = batteryUnavailable,
                    signInBlocked = GoogleMessagesLink.signInBlocked(context), localNotice = localNotice,
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
                    }, onReconnect = { GoogleMessagesLinkService.start(context); refreshAfterAction() },
                    onBattery = { if (batteryAllowed) openBatterySettings() else sheet = "battery" },
                    onAllowNotifications = {
                        if (Build.VERSION.SDK_INT >= 33) {
                            if (notificationAsked) context.startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName))
                            else { notificationAsked = true; notificationRequest.launch(Manifest.permission.POST_NOTIFICATIONS) }
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
            val handle = (result as? LinkOpenResult.Open)?.handle
            sheet = null
            if (handle != null && LinkRuntime.beginPairing()) {
                GoogleMessagesLinkService.start(context)
                busy = true
                scope.launch {
                    try { withContext(Dispatchers.IO) { handle.relink() } }
                    catch (error: kotlinx.coroutines.CancellationException) { throw error }
                    catch (_: Exception) { localNotice = "relink_failed" }
                    finally { LinkRuntime.endPairing(); busy = false; refreshAfterAction() }
                }
            }
        }, stringResource(R.string.peppy_google_messages_link_relink_confirm), showPrivacyPolicy = false)
        "unlink" -> Confirmation("unlink", R.string.peppy_google_messages_link_unlink_title, R.string.peppy_google_messages_link_unlink_body, R.string.peppy_google_messages_link_unlink_confirm, { sheet = null }) {
            val handle = (result as? LinkOpenResult.Open)?.handle ?: return@Confirmation
            busy = true; scope.launch {
                try { withContext(Dispatchers.IO) { handle.unlink() }; localNotice = "unlinked_done" }
                catch (error: kotlinx.coroutines.CancellationException) { throw error }
                catch (_: Exception) { localNotice = "unlink_remote_failed" }
                finally { busy = false; refreshAfterAction() }
            }
        }
        "remove", "reset" -> Confirmation(sheet ?: "reset", if (sheet == "remove") R.string.peppy_google_messages_link_remove_title else R.string.peppy_google_messages_link_reset_title, if (sheet == "remove") R.string.peppy_google_messages_link_remove_body else R.string.peppy_google_messages_link_reset_body, if (sheet == "remove") R.string.peppy_google_messages_link_remove_confirm else R.string.peppy_google_messages_link_reset_confirm, { sheet = null }) {
            busy = true; scope.launch {
                try { withContext(Dispatchers.IO) { LinkRuntime.resetLocalState(context.applicationContext) }; localNotice = "removed" }
                catch (error: kotlinx.coroutines.CancellationException) { throw error }
                catch (_: Exception) { localNotice = "unknown" }
                finally { busy = false; refreshAfterAction() }
            }
        }
        "battery" -> ConsentSheet(stringResource(R.string.peppy_google_messages_link_battery_title), stringResource(R.string.peppy_google_messages_link_battery_sheet_body), "google-messages-link-battery-sheet", { sheet = null }, { sheet = null; openBatterySettings() }, stringResource(R.string.peppy_toggle_android_open_settings), showPrivacyPolicy = false)
    }
}

@Composable private fun Loading() = Column(Modifier.testTag("google-messages-link-loading")) { CircularProgressIndicator(); Text(stringResource(R.string.peppy_google_messages_link_loading), Modifier.semantics { liveRegion = LiveRegionMode.Polite }) }

@Composable private fun UnreadablePanel(onReset: () -> Unit, busy: Boolean) {
    StatusRow("google-messages-link-status", stringResource(R.string.peppy_google_messages_link_status_label), stringResource(R.string.peppy_google_messages_link_status_unreadable))
    Text(stringResource(R.string.peppy_google_messages_link_unreadable_body), Modifier.testTag("google-messages-link-unreadable"))
    PanelButton(stringResource(R.string.peppy_google_messages_link_action_reset), !busy, "google-messages-link-reset", onReset)
}

@Composable private fun StatusPanel(status: LinkStatus, emoji: String?, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, signInBlocked: Boolean, localNotice: String?, dismissedSeq: Long?, onDismissNotice: () -> Unit, onStart: () -> Unit, onRelink: () -> Unit, onUnlink: () -> Unit, onRemove: () -> Unit, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit) {
    val statusText = when (status.state) { "unlinked" -> R.string.peppy_google_messages_link_status_unlinked; "pairing" -> R.string.peppy_google_messages_link_status_pairing; "linked" -> R.string.peppy_google_messages_link_status_linked; "needs_relink" -> R.string.peppy_google_messages_link_status_needs_relink; "signed_out" -> R.string.peppy_google_messages_link_status_signed_out; else -> R.string.peppy_google_messages_link_load_failed }
    if (localNotice != null || (status.stopCode.isNotEmpty() && dismissedSeq != status.seq)) Notice(localNotice ?: status.stopCode, onDismissNotice)
    StatusRow("google-messages-link-status", stringResource(R.string.peppy_google_messages_link_status_label), stringResource(statusText))
    if (busy) Text(stringResource(R.string.peppy_google_messages_link_working), Modifier.semantics { liveRegion = LiveRegionMode.Polite })
    when (status.state) {
        "unlinked" -> { Text(stringResource(R.string.peppy_google_messages_link_unlinked_body), Modifier.testTag("google-messages-link-unlinked")); Text(stringResource(R.string.peppy_google_messages_link_replaces_browser)); if (signInBlocked) Text(stringResource(R.string.peppy_google_messages_link_start_blocked)) else PanelButton(stringResource(R.string.peppy_google_messages_link_action_link), !busy && !LinkRuntime.pairingInProgress.get(), "google-messages-link-start", onStart) }
        "pairing" -> Pairing(emoji, notificationsOff, onAllowNotifications)
        "linked" -> Linked(status, busy, checkResult, notificationsOff, batteryAllowed, batteryUnavailable, onCheck, onReconnect, onBattery, onAllowNotifications, onUnlink)
        "needs_relink" -> { Text(stringResource(R.string.peppy_google_messages_link_needs_relink_body), Modifier.testTag("google-messages-link-needs-relink")); PanelButton(stringResource(R.string.peppy_google_messages_link_action_relink), !busy && !LinkRuntime.pairingInProgress.get(), "google-messages-link-relink", onRelink); Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy, "google-messages-link-unlink", onUnlink); TextButton(onClick = onRemove, enabled = !busy, modifier = Modifier.testTag("google-messages-link-remove-local")) { Text(stringResource(R.string.peppy_google_messages_link_action_remove_local)) } }
        "signed_out" -> { Text(stringResource(R.string.peppy_google_messages_link_signed_out_body), Modifier.testTag("google-messages-link-signed-out")); PanelButton(stringResource(R.string.peppy_google_messages_link_action_sign_in_again), !busy && !LinkRuntime.pairingInProgress.get(), "google-messages-link-sign-in-again", onStart); TextButton(onClick = onRemove, enabled = !busy, modifier = Modifier.testTag("google-messages-link-remove-local")) { Text(stringResource(R.string.peppy_google_messages_link_action_remove_local)) } }
    }
}

@Composable private fun Pairing(emoji: String?, notificationsOff: Boolean, onAllowNotifications: () -> Unit) { if (emoji == null) Text(stringResource(R.string.peppy_google_messages_link_pairing_connecting), Modifier.testTag("google-messages-link-pairing").semantics { liveRegion = LiveRegionMode.Polite }) else Column(Modifier.testTag("google-messages-link-emoji-card").semantics { liveRegion = LiveRegionMode.Assertive }) { Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_instruction)); Text(emoji, Modifier.testTag("google-messages-link-emoji"), style = MaterialTheme.typography.displayLarge); Text(stringResource(R.string.peppy_google_messages_link_pairing_emoji_hint)) }; if (notificationsOff) Notifications(onAllowNotifications) }

@Composable private fun Linked(status: LinkStatus, busy: Boolean, checkResult: Int?, notificationsOff: Boolean, batteryAllowed: Boolean, batteryUnavailable: Boolean, onCheck: () -> Unit, onReconnect: () -> Unit, onBattery: () -> Unit, onAllowNotifications: () -> Unit, onUnlink: () -> Unit) { Text(stringResource(R.string.peppy_google_messages_link_linked_body), Modifier.testTag("google-messages-link-linked")); val connection = when { !status.running -> R.string.peppy_google_messages_link_connection_not_running; !status.phoneResponding -> R.string.peppy_google_messages_link_connection_phone_not_responding; !status.healthy -> R.string.peppy_google_messages_link_connection_unconfirmed; else -> R.string.peppy_google_messages_link_connection_connected }; StatusRow("google-messages-link-connection", stringResource(R.string.peppy_google_messages_link_connection_label), stringResource(connection)); StatusRow("google-messages-link-last-ok", stringResource(R.string.peppy_google_messages_link_last_ok_label), if (status.lastOkMs == 0L) stringResource(R.string.peppy_google_messages_link_last_ok_never) else DateUtils.getRelativeTimeSpanString(status.lastOkMs).toString()); if (!status.running) { Text(stringResource(R.string.peppy_google_messages_link_help_not_running)); TextButton(onClick = onReconnect, modifier = Modifier.testTag("google-messages-link-reconnect")) { Text(stringResource(R.string.peppy_google_messages_link_action_reconnect)) } } else { if (!status.phoneResponding) Text(stringResource(R.string.peppy_google_messages_link_help_phone_not_responding)) else if (!status.healthy) Text(stringResource(R.string.peppy_google_messages_link_help_unconfirmed)); PanelButton(if (busy) stringResource(R.string.peppy_google_messages_link_checking) else stringResource(R.string.peppy_google_messages_link_action_check_now), !busy, "google-messages-link-check-now", onCheck) }; checkResult?.let { Text(stringResource(if (it >= 0) R.string.peppy_google_messages_link_check_ok else R.string.peppy_google_messages_link_check_failed), Modifier.testTag("google-messages-link-check-result")) }; SettingToggleRow(stringResource(R.string.peppy_google_messages_link_battery_title), batteryAllowed, true, { onBattery() }, "google-messages-link-battery-row", stringResource(R.string.peppy_google_messages_link_battery_subtitle), stringResource(R.string.peppy_toggle_android_open_settings), stringResource(if (batteryAllowed) R.string.peppy_toggle_android_battery_allowed else R.string.peppy_toggle_android_battery_delayed)); if (batteryUnavailable) Text(stringResource(R.string.peppy_toggle_android_battery_unavailable), Modifier.testTag("google-messages-link-battery-unavailable")); if (notificationsOff) Notifications(onAllowNotifications); Secondary(stringResource(R.string.peppy_google_messages_link_action_unlink), !busy, "google-messages-link-unlink", onUnlink) }

@Composable private fun Notifications(onAllow: () -> Unit) = Column(Modifier.testTag("google-messages-link-notifications-row")) { Text(stringResource(R.string.peppy_google_messages_link_notifications_off)); TextButton(onClick = onAllow, modifier = Modifier.testTag("google-messages-link-notifications-allow")) { Text(stringResource(R.string.peppy_remote_messaging_allow_notifications)) } }
@Composable private fun Notice(code: String, dismiss: () -> Unit) {
    val text = when (code) {
        "removed" -> R.string.peppy_google_messages_link_notice_removed_local
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
    val polite = code in setOf("removed", "pair_cancelled")
    Column(Modifier.testTag("google-messages-link-notice").semantics { liveRegion = if (polite) LiveRegionMode.Polite else LiveRegionMode.Assertive }) { Text(stringResource(text)); TextButton(onClick = dismiss, modifier = Modifier.testTag("google-messages-link-notice-dismiss")) { Text(stringResource(R.string.peppy_google_messages_link_notice_dismiss)) } }
}
@Composable private fun PanelButton(label: String, enabled: Boolean, tag: String, onClick: () -> Unit) = Button(onClick = onClick, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag)) { Text(label) }
@Composable private fun Secondary(label: String, enabled: Boolean, tag: String, onClick: () -> Unit) = OutlinedButton(onClick = onClick, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag)) { Text(label) }
@Composable private fun Confirmation(kind: String, title: Int, body: Int, action: Int, onClose: () -> Unit, onConfirm: () -> Unit) = ConsentSheet(stringResource(title), stringResource(body), "google-messages-link-$kind-sheet", onClose, { onClose(); onConfirm() }, stringResource(action), nextStep = stringResource(R.string.peppy_google_messages_link_manual_removal_steps), showPrivacyPolicy = false)
