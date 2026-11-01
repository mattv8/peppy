package dev.peppy.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.annotation.StringRes
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.ui.theme.PeppyTokens
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.NativeAccountNoticeKind
import uniffi.peppy_mobile_bindings.NativeNoticeSeverity

/** The one global ribbon's view model. Severity picks colours; the text is already resolved. */
internal data class SystemRibbonModel(
    /** Stable per notice kind; the expanded body state resets when it changes. */
    val key: String,
    val severity: NativeNoticeSeverity,
    val title: String,
    val body: String,
    /** Null when there is no validated URL, so the ribbon has no action. */
    val actionLabel: String?,
    val manageUrl: String?,
    val dismissible: Boolean,
)

/** Copy keys for one kind. Android phones are gateways, so only the gateway body variants are used. */
private class AccountCopy(
    @StringRes val title: Int,
    @StringRes val gatewayBody: Int,
    @StringRes val action: Int,
    @StringRes val gatewayBodyNoDate: Int = 0,
)

private fun accountCopy(kind: NativeAccountNoticeKind): AccountCopy = when (kind) {
    NativeAccountNoticeKind.PAYMENT_FAILED_GRACE -> AccountCopy(
        R.string.peppy_account_payment_failed_grace_title,
        R.string.peppy_account_payment_failed_grace_body_gateway,
        R.string.peppy_account_payment_failed_grace_action,
        R.string.peppy_account_payment_failed_grace_body_gateway_nodate,
    )
    NativeAccountNoticeKind.ENDING_SCHEDULED -> AccountCopy(
        R.string.peppy_account_ending_scheduled_title,
        R.string.peppy_account_ending_scheduled_body_gateway,
        R.string.peppy_account_ending_scheduled_action,
    )
    NativeAccountNoticeKind.READ_ONLY_PAST_DUE -> AccountCopy(
        R.string.peppy_account_read_only_past_due_title,
        R.string.peppy_account_read_only_past_due_body_gateway,
        R.string.peppy_account_read_only_past_due_action,
    )
    NativeAccountNoticeKind.READ_ONLY_CANCELED -> AccountCopy(
        R.string.peppy_account_read_only_canceled_title,
        R.string.peppy_account_read_only_canceled_body_gateway,
        R.string.peppy_account_read_only_canceled_action,
    )
    NativeAccountNoticeKind.READ_ONLY_INCOMPLETE -> AccountCopy(
        R.string.peppy_account_read_only_incomplete_title,
        R.string.peppy_account_read_only_incomplete_body_gateway,
        R.string.peppy_account_read_only_incomplete_action,
    )
    NativeAccountNoticeKind.READ_ONLY_NO_SUBSCRIPTION -> AccountCopy(
        R.string.peppy_account_read_only_no_subscription_title,
        R.string.peppy_account_read_only_no_subscription_body_gateway,
        R.string.peppy_account_read_only_no_subscription_action,
    )
    NativeAccountNoticeKind.READ_ONLY_TESTER_INELIGIBLE -> AccountCopy(
        R.string.peppy_account_read_only_tester_ineligible_title,
        R.string.peppy_account_read_only_tester_ineligible_body_gateway,
        R.string.peppy_account_read_only_tester_ineligible_action,
    )
    NativeAccountNoticeKind.READ_ONLY_ACCOUNT_DELETING -> AccountCopy(
        R.string.peppy_account_read_only_account_deleting_title,
        R.string.peppy_account_read_only_account_deleting_body_gateway,
        R.string.peppy_account_read_only_account_deleting_action,
    )
    NativeAccountNoticeKind.READ_ONLY_OTHER -> AccountCopy(
        R.string.peppy_account_read_only_other_title,
        R.string.peppy_account_read_only_other_body_gateway,
        R.string.peppy_account_read_only_other_action,
    )
}

/** Medium date in the device's time zone; the caller only formats a deadline that is still in the future. */
private fun formatDeadline(deadlineSeconds: Long): String =
    DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(deadlineSeconds * 1000))

/** The notice title as shown on the ribbon, also used inside the account notification body. */
internal fun accountRibbonTitle(context: Context, notice: CachedAccountNotice, nowMs: Long = System.currentTimeMillis()): String {
    val copy = accountCopy(notice.kind)
    val date = notice.deadline?.takeIf { it * 1000 > nowMs }?.let(::formatDeadline).orEmpty()
    return if (notice.kind == NativeAccountNoticeKind.ENDING_SCHEDULED) context.getString(copy.title, date) else context.getString(copy.title)
}

internal fun buildSystemRibbonModel(context: Context, slot: RibbonSlot, nowMs: Long): SystemRibbonModel = when (slot) {
    RibbonSlot.ServerOffline -> SystemRibbonModel(
        key = "server_offline",
        severity = NativeNoticeSeverity.WARNING,
        title = context.getString(R.string.peppy_server_offline_title),
        body = context.getString(R.string.peppy_server_offline_body),
        actionLabel = null,
        manageUrl = null,
        dismissible = false,
    )
    is RibbonSlot.Account -> {
        val notice = slot.notice
        val copy = accountCopy(notice.kind)
        val deadline = notice.deadline?.takeIf { it * 1000 > nowMs }
        val date = deadline?.let(::formatDeadline).orEmpty()
        val bodyRes = if (notice.kind == NativeAccountNoticeKind.PAYMENT_FAILED_GRACE && deadline == null) copy.gatewayBodyNoDate else copy.gatewayBody
        SystemRibbonModel(
            key = notice.kind.name,
            severity = notice.severity,
            title = accountRibbonTitle(context, notice, nowMs),
            body = context.getString(bodyRes, date),
            actionLabel = notice.manageUrl?.let { context.getString(copy.action) },
            manageUrl = notice.manageUrl,
            dismissible = notice.dismissible,
        )
    }
}

/** Stateless ribbon. Warning and error differ in colour and border width; the title always names the state. */
@Composable
internal fun SystemRibbon(model: SystemRibbonModel, linkFailed: Boolean, onAction: (String) -> Unit, onDismiss: () -> Unit) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    val isError = model.severity == NativeNoticeSeverity.ERROR
    val surface = if (isError) colors.errorSurface else colors.warningSurface
    // Error keeps coral for the border only; its text uses the strong and primary text colours for contrast.
    val titleColor = if (isError) colors.textStrong else colors.warningText
    val bodyColor = if (isError) colors.textPrimary else colors.warningText
    val border = if (isError) colors.error else colors.warningBorder
    val borderWidth = if (isError) 2.dp else 1.dp
    val opensInBrowser = stringResource(R.string.peppy_system_banner_opens_in_browser)
    var expanded by remember(model.key) { mutableStateOf(false) }
    var overflows by remember(model.key, model.body) { mutableStateOf(false) }
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("system-ribbon").semantics { liveRegion = LiveRegionMode.Polite }
            .drawWithContent {
                drawContent()
                // Bottom edge only: a single rule whose width carries the severity.
                val y = size.height - borderWidth.toPx() / 2
                drawLine(border, Offset(0f, y), Offset(size.width, y), strokeWidth = borderWidth.toPx())
            },
        color = surface,
        contentColor = titleColor,
    ) {
        // Status-bar padding sits inside the surface so the ribbon colour tints the status bar.
        Column(Modifier.statusBarsPadding().padding(horizontal = 16.dp, vertical = 12.dp)) {
            Text(model.title, style = MaterialTheme.typography.titleSmall, color = titleColor, modifier = Modifier.testTag("system-ribbon-title"))
            Text(
                model.body,
                style = MaterialTheme.typography.bodySmall,
                color = bodyColor,
                maxLines = if (expanded) Int.MAX_VALUE else BODY_COLLAPSED_LINES,
                overflow = TextOverflow.Ellipsis,
                onTextLayout = { if (!expanded) overflows = it.hasVisualOverflow },
                modifier = Modifier.testTag("system-ribbon-body"),
            )
            if (linkFailed) Text(
                stringResource(R.string.peppy_toggle_android_link_unavailable),
                style = MaterialTheme.typography.bodySmall,
                color = bodyColor,
                modifier = Modifier.testTag("system-ribbon-link-error"),
            )
            if (overflows || expanded) TextButton(
                onClick = { expanded = !expanded },
                colors = ButtonDefaults.textButtonColors(contentColor = titleColor),
                modifier = Modifier.heightIn(min = 48.dp).testTag("system-ribbon-toggle"),
            ) { Text(stringResource(if (expanded) R.string.peppy_system_banner_less else R.string.peppy_system_banner_more)) }
            if (model.actionLabel != null || model.dismissible) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    model.actionLabel?.let { label ->
                        TextButton(
                            onClick = { model.manageUrl?.let(onAction) },
                            colors = ButtonDefaults.textButtonColors(contentColor = titleColor),
                            modifier = Modifier.heightIn(min = 48.dp).testTag("system-ribbon-action")
                                .semantics { contentDescription = "$label, $opensInBrowser" },
                        ) { Text(label) }
                    }
                    if (model.dismissible) TextButton(
                        onClick = onDismiss,
                        colors = ButtonDefaults.textButtonColors(contentColor = titleColor),
                        modifier = Modifier.heightIn(min = 48.dp).testTag("system-ribbon-dismiss"),
                    ) { Text(stringResource(R.string.peppy_system_banner_dismiss)) }
                }
            }
        }
    }
}

private const val BODY_COLLAPSED_LINES = 3

/**
 * The one global ribbon slot: server offline, then account read-only, then account warning. It
 * shows only for an enrolled device, and server offline only with vault keys ready (as before).
 * Every NativeGateway or account read runs on [Dispatchers.IO]. Returns whether the ribbon is
 * shown, so content below it can consume the status-bar inset the ribbon already pads.
 */
@Composable
internal fun SystemRibbonHost(enrolled: Boolean, unlocked: Boolean): Boolean {
    if (!enrolled) return false
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val lifecycleOwner = LocalLifecycleOwner.current
    val reachability by ServerReachabilityState.state.collectAsState()
    val access by AccountAccessState.access.collectAsState()
    val snoozes by AccountAccessState.snoozes.collectAsState()
    var linkFailed by remember(access?.notice) { mutableStateOf(false) }

    // The lifecycle observer below fires ON_RESUME as soon as it is added, so it covers launch too.
    LaunchedEffect(Unit) { withContext(Dispatchers.IO) { AccountAccessState.ensureLoaded(context) } }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                scope.launch(Dispatchers.IO) { AccountAccessState.refresh(context, AccountRefreshTrigger.LAUNCH_OR_FOREGROUND) }
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val now = System.currentTimeMillis()
    val slot = resolveRibbonSlot(
        serverOffline = unlocked && reachability == ServerReachability.UNAVAILABLE,
        notice = access?.notice,
        snoozedKeys = activeSnoozeKeys(snoozes, now),
        nowMs = now,
    ) ?: return false
    val model = buildSystemRibbonModel(context, slot, now)
    SystemRibbon(
        model = model,
        linkFailed = linkFailed,
        onAction = { url -> if (!openManageUrl(context, url)) linkFailed = true },
        onDismiss = {
            (slot as? RibbonSlot.Account)?.notice?.let { notice ->
                scope.launch(Dispatchers.IO) { AccountAccessState.snooze(context, notice) }
            }
        },
    )
    return true
}

/** Drops the status-bar inset from content under a visible ribbon, which already pads it. */
@Composable
internal fun Modifier.belowSystemRibbon(ribbonVisible: Boolean): Modifier =
    if (ribbonVisible) consumeWindowInsets(WindowInsets.statusBars) else this

/** Opens only the Rust-validated manage URL in the system browser; returns false when no app can open it. */
private fun openManageUrl(context: Context, url: String): Boolean = try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    true
} catch (_: ActivityNotFoundException) {
    false
}
