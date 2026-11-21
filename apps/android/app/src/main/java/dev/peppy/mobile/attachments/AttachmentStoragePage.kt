package dev.peppy.mobile.attachments

import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.AttentionCallout
import dev.peppy.mobile.ui.DetailPage
import dev.peppy.mobile.ui.ExternalLinkRow
import dev.peppy.mobile.ui.InfoSpec
import dev.peppy.mobile.ui.PageAction
import dev.peppy.mobile.ui.PageChrome
import dev.peppy.mobile.ui.SettingsGroup
import dev.peppy.mobile.ui.theme.PeppyColorScheme
import dev.peppy.mobile.ui.theme.PeppyTokens
import java.util.Locale

internal enum class StorageBarLevel { NORMAL, WARNING, FULL }

internal fun storageBarLevel(used: Long, limit: Long): StorageBarLevel? = when {
    limit <= 0 || used < 0 -> null
    used >= limit -> StorageBarLevel.FULL
    used * 10 >= limit * 9 -> StorageBarLevel.WARNING
    else -> StorageBarLevel.NORMAL
}

@Composable
internal fun AttachmentStoragePage(
    state: AttachmentStorageUiState,
    scope: String?,
    busy: Boolean,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onManage: (String) -> Unit,
    showPurchaseActions: Boolean,
) {
    var infoOpen by remember(scope) { mutableStateOf(false) }
    val refresh = PageAction(
        icon = Icons.Default.Refresh,
        label = stringResource(R.string.peppy_attachment_storage_refresh),
        tag = "attachment-storage-refresh",
        enabled = !busy,
        onClick = onRefresh,
    )
    val info = PageAction(
        icon = Icons.Default.Info,
        label = stringResource(R.string.peppy_attachment_storage_info_label),
        tag = "attachment-storage-info-button",
        onClick = { infoOpen = true },
    )
    val actions = if (state is AttachmentStorageUiState.Available) listOf(info, refresh) else listOf(refresh)
    DetailPage(
        PageChrome(
            title = stringResource(R.string.peppy_account_storage_title),
            tag = "attachment-storage",
            onBack = onBack,
            actions = actions,
        ),
    ) {
        ProgressSlot(active = state == AttachmentStorageUiState.Loading || busy)
        when (state) {
            AttachmentStorageUiState.Loading -> LoadingStorage()
            AttachmentStorageUiState.Unsupported -> Text(
                stringResource(R.string.peppy_attachment_storage_unavailable),
                Modifier.testTag("attachment-storage-unavailable"),
                style = MaterialTheme.typography.bodyMedium,
            )
            is AttachmentStorageUiState.Unavailable -> UnavailableStorage(state.lastKnown, busy, onRefresh)
            is AttachmentStorageUiState.Available -> AvailableStorage(
                status = state.status,
                showPurchaseActions = showPurchaseActions,
                onLearnMore = { infoOpen = true },
                onManage = onManage,
            )
        }
    }
    if (infoOpen && state is AttachmentStorageUiState.Available) {
        StorageInfoDialog(showPurchaseActions, onDismiss = { infoOpen = false })
    }
}

@Composable
private fun ProgressSlot(active: Boolean) {
    val description = stringResource(R.string.peppy_about_loading)
    Box(Modifier.fillMaxWidth().height(4.dp).testTag("attachment-storage-progress-slot")) {
        if (active) {
            LinearProgressIndicator(
                Modifier
                    .fillMaxWidth()
                    .testTag("attachment-storage-progress")
                    .semantics { contentDescription = description },
            )
        }
    }
}

@Composable
private fun LoadingStorage() {
    Text(
        stringResource(R.string.peppy_attachment_storage_loading),
        Modifier.testTag("attachment-storage-loading"),
        style = MaterialTheme.typography.bodyMedium,
    )
}

@Composable
private fun UnavailableStorage(lastKnown: AttachmentStorageSnapshot?, busy: Boolean, onRefresh: () -> Unit) {
    AttentionCallout(
        tag = "attachment-storage-unavailable",
        text = stringResource(R.string.peppy_attachment_storage_unavailable),
        actionLabel = stringResource(R.string.peppy_attachment_storage_reload_cta),
        onAction = { if (!busy) onRefresh() },
    )
    lastKnown?.let { stale ->
        Text(
            stringResource(R.string.peppy_attachment_storage_stale_usage),
            Modifier.testTag("attachment-storage-stale-label"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        QuotaBlock(stale, prefix = "attachment-storage-stale", muted = true)
    }
}

@Composable
private fun AvailableStorage(
    status: AttachmentStorageSnapshot,
    showPurchaseActions: Boolean,
    onLearnMore: () -> Unit,
    onManage: (String) -> Unit,
) {
    CapacityNotice(attachmentStorageCapacityNotice(status), onLearnMore)
    QuotaBlock(status, prefix = "attachment-storage", muted = false)
    if (showPurchaseActions) {
        SettingsGroup(tag = "attachment-storage-manage-group", title = null) {
            ExternalLinkRow(
                tag = "attachment-storage-manage",
                label = stringResource(R.string.peppy_attachment_storage_manage_cta),
                info = InfoSpec(
                    title = stringResource(R.string.peppy_attachment_storage_manage_cta),
                    body = stringResource(R.string.peppy_attachment_storage_billing_handoff),
                    tag = "info-attachment-storage-manage",
                ),
                onClick = { onManage("https://peppy.pro") },
            )
        }
    }
}

@Composable
private fun CapacityNotice(notice: AttachmentStorageCapacityNotice, onLearnMore: () -> Unit) {
    when (notice) {
        AttachmentStorageCapacityNotice.None -> Unit
        AttachmentStorageCapacityNotice.NearFull -> CapacityCallout(R.string.peppy_attachment_storage_near_full, onLearnMore)
        AttachmentStorageCapacityNotice.FullCleanup -> CapacityCallout(R.string.peppy_attachment_storage_full_cleanup, onLearnMore)
        is AttachmentStorageCapacityNotice.NearFullBlocked -> {
            CapacityCallout(R.string.peppy_attachment_storage_near_full, onLearnMore)
            CauseText(notice.causeKey.causeResourceId())
        }
        is AttachmentStorageCapacityNotice.Cause -> CauseText(notice.key.causeResourceId())
    }
}

@Composable
private fun CapacityCallout(textResId: Int, onLearnMore: () -> Unit) {
    AttentionCallout(
        tag = "attachment-storage-capacity-notice",
        text = stringResource(textResId),
        actionLabel = stringResource(R.string.peppy_attachment_storage_learn_more),
        onAction = onLearnMore,
    )
}

@Composable
private fun CauseText(textResId: Int) {
    Text(
        stringResource(textResId),
        Modifier.testTag("attachment-storage-cause"),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun QuotaBlock(status: AttachmentStorageSnapshot, prefix: String, muted: Boolean) {
    val textColor = if (muted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface
    Column(Modifier.testTag("$prefix-values"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            stringResource(
                R.string.peppy_attachment_storage_used_of_limit,
                storageBytes(status.usedBytes),
                storageBytes(status.limitBytes),
            ),
            Modifier.testTag("$prefix-used-of-limit"),
            style = MaterialTheme.typography.titleMedium,
            color = textColor,
        )
        storageBarLevel(status.usedBytes, status.limitBytes)?.let { level ->
            UsageBar(
                fraction = (status.usedBytes.toDouble() / status.limitBytes).coerceIn(0.0, 1.0).toFloat(),
                color = barColor(level, muted),
                tag = "$prefix-bar",
            )
        }
        Text(
            breakdownText(status),
            Modifier.testTag("$prefix-breakdown"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun breakdownText(status: AttachmentStorageSnapshot): String {
    val base = storageBytes(status.baseBytes)
    return if (status.limitBytes <= status.baseBytes) {
        stringResource(R.string.peppy_attachment_storage_included_only, base)
    } else {
        stringResource(
            R.string.peppy_attachment_storage_included_plus_addon,
            base,
            storageBytes(status.limitBytes - status.baseBytes),
        )
    }
}

@Composable
private fun UsageBar(fraction: Float, color: Color, tag: String) {
    Box(
        Modifier
            .fillMaxWidth()
            .height(6.dp)
            .clip(RoundedCornerShape(3.dp))
            .background(storageTokens().surfaceActive)
            .testTag(tag)
            .semantics { hideFromAccessibility() },
    ) {
        Box(Modifier.fillMaxWidth(fraction).fillMaxHeight().background(color))
    }
}

@Composable
private fun barColor(level: StorageBarLevel, muted: Boolean): Color {
    val tokens = storageTokens()
    return when {
        muted -> tokens.textMuted
        level == StorageBarLevel.NORMAL -> MaterialTheme.colorScheme.primary
        level == StorageBarLevel.WARNING -> tokens.warningText
        else -> MaterialTheme.colorScheme.error
    }
}

@Composable
private fun storageTokens(): PeppyColorScheme = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light

@Composable
private fun StorageInfoDialog(includePurchaseInfo: Boolean, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.peppy_attachment_storage_info_title), Modifier.semantics { heading() }) },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(stringResource(R.string.peppy_attachment_storage_info_body_cleanup), style = MaterialTheme.typography.bodyMedium)
                Text(stringResource(R.string.peppy_attachment_storage_info_body_protected), style = MaterialTheme.typography.bodyMedium)
                if (includePurchaseInfo) Text(stringResource(R.string.peppy_attachment_storage_info_body_purchase), style = MaterialTheme.typography.bodyMedium)
            }
        },
        confirmButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.heightIn(min = 48.dp).testTag("attachment-storage-info-dismiss"),
            ) { Text(stringResource(R.string.peppy_attachment_storage_info_dismiss)) }
        },
        modifier = Modifier.testTag("attachment-storage-info-dialog"),
    )
}

private fun String.causeResourceId() = when (this) {
    "peppy.attachment_storage_cleanup_pending" -> R.string.peppy_attachment_storage_cleanup_pending
    "peppy.attachment_storage_device_update_required" -> R.string.peppy_attachment_storage_device_update_required
    "peppy.attachment_storage_read_only" -> R.string.peppy_attachment_storage_read_only
    "peppy.attachment_storage_billing_pending" -> R.string.peppy_attachment_storage_billing_pending
    "peppy.attachment_storage_billing_unavailable" -> R.string.peppy_attachment_storage_billing_unavailable
    "peppy.attachment_storage_disabled" -> R.string.peppy_attachment_storage_disabled
    else -> R.string.peppy_attachment_storage_unavailable
}

private fun storageBytes(bytes: Long) = when {
    bytes < 1_000_000L -> "$bytes B"
    bytes < 1_000_000_000L -> "%.1f MB".format(Locale.getDefault(), bytes / 1_000_000.0)
    else -> "%.1f GB".format(Locale.getDefault(), bytes / 1_000_000_000.0)
}
