package dev.peppy.mobile.gateway

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.PhoneIphone
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalResources
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.collapse
import androidx.compose.ui.semantics.expand
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.BadgeSpec
import dev.peppy.mobile.ui.BadgeTone
import dev.peppy.mobile.ui.DetailPage
import dev.peppy.mobile.ui.InfoButton
import dev.peppy.mobile.ui.PageAction
import dev.peppy.mobile.ui.PageChrome
import dev.peppy.mobile.ui.SettingsGroup
import dev.peppy.mobile.ui.SingleLineBadges
import kotlinx.coroutines.launch

internal enum class PresenceBadge { CONNECTED, SEEN_NOW, SEEN_MINUTES, SEEN_HOURS, NEVER, UNKNOWN }

/** Returns the badge and its minutes or hours value for [PresenceBadge.SEEN_MINUTES] / [PresenceBadge.SEEN_HOURS]. */
internal fun presenceBadge(presence: DevicePresence?): Pair<PresenceBadge, Long?> {
    if (presence == null) return PresenceBadge.UNKNOWN to null
    return when (presence.state) {
        DevicePresenceState.CONNECTED -> PresenceBadge.CONNECTED to null
        DevicePresenceState.NEVER -> PresenceBadge.NEVER to null
        DevicePresenceState.UNKNOWN -> PresenceBadge.UNKNOWN to null
        DevicePresenceState.SEEN -> {
            val age = presence.lastSeenAgeSeconds ?: return PresenceBadge.UNKNOWN to null
            when {
                age < 60 -> PresenceBadge.SEEN_NOW to null
                age < 3600 -> PresenceBadge.SEEN_MINUTES to age / 60
                else -> PresenceBadge.SEEN_HOURS to age / 3600
            }
        }
    }
}

private enum class DeviceAction { RENAME, REMOVE, DISCONNECT }

private data class DeviceDialog(val action: DeviceAction, val deviceId: String)

private fun canRename(self: GatewayAccountDevice, device: GatewayAccountDevice) = !self.revoked && !device.revoked

private fun canRemove(self: GatewayAccountDevice, device: GatewayAccountDevice) =
    self.role == "owner" && !self.revoked && device.id != self.id && !device.revoked

private fun canDisconnect(self: GatewayAccountDevice, device: GatewayAccountDevice) = device.id == self.id && !device.revoked

private fun isAllowed(request: DeviceDialog, state: GatewayAccountState): Boolean {
    val device = state.devices.firstOrNull { it.id == request.deviceId } ?: return false
    return when (request.action) {
        DeviceAction.RENAME -> canRename(state.self, device)
        DeviceAction.REMOVE -> canRemove(state.self, device)
        DeviceAction.DISCONNECT -> canDisconnect(state.self, device)
    }
}

@Composable
internal fun DevicesPage(
    state: GatewayAccountState,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    refreshing: Boolean,
    revoke: suspend (GatewayAccountDevice) -> Boolean,
    rename: suspend (GatewayAccountDevice, String) -> RenameResult,
    onChanged: () -> Unit,
    accountReadOnly: Boolean = false,
    errorMessage: String? = null,
) {
    val self = state.self
    val unnamed = stringResource(R.string.peppy_devices_unnamed)
    val scope = rememberCoroutineScope()
    val latest by rememberUpdatedState(state)
    var dialog by remember { mutableStateOf<DeviceDialog?>(null) }
    var busy by remember { mutableStateOf(false) }
    var removedExpanded by rememberSaveable { mutableStateOf(false) }

    // Runs one dialog call at most once. busy is set before launch, so a second activation before
    // recomposition sees it and returns. Eligibility is checked against the latest roster.
    fun submit(request: DeviceDialog, call: suspend (GatewayAccountDevice) -> Unit) {
        if (busy || !isAllowed(request, latest)) return
        val device = latest.devices.first { it.id == request.deviceId }
        busy = true
        scope.launch {
            try {
                call(device)
            } finally {
                busy = false
            }
        }
    }

    LaunchedEffect(state, busy) {
        val request = dialog
        if (request != null && !busy && !isAllowed(request, state)) dialog = null
    }

    DetailPage(
        PageChrome(
            title = stringResource(R.string.peppy_devices),
            tag = "devices",
            onBack = onBack,
            actions = listOf(
                PageAction(
                    icon = Icons.Default.Refresh,
                    label = stringResource(R.string.peppy_devices_refresh),
                    tag = "devices-refresh",
                    enabled = !refreshing,
                    onClick = onRefresh,
                ),
            ),
        ),
    ) {
        Box(Modifier.fillMaxWidth().height(4.dp)) {
            if (refreshing) {
                LinearProgressIndicator(Modifier.fillMaxWidth().testTag("devices-refreshing"))
            }
        }
        if (accountReadOnly) {
            Text(
                stringResource(R.string.peppy_devices_read_only_caption),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag("devices-read-only-caption"),
            )
        }
        errorMessage?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag("devices-error").semantics { liveRegion = LiveRegionMode.Polite },
            )
        }
        if (self.role != "owner") {
            Row(
                Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("devices-owner-required"),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    stringResource(R.string.peppy_account_owner_required_short),
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                InfoButton(
                    topic = stringResource(R.string.peppy_devices),
                    title = stringResource(R.string.peppy_devices),
                    body = stringResource(R.string.peppy_devices_owner_required_help),
                    tag = "info-devices-owner-required",
                )
            }
        }
        val active = state.devices.filter { !it.revoked }
        val removed = state.devices.filter { it.revoked }
        if (state.devices.isEmpty()) {
            Text(
                stringResource(R.string.peppy_devices_empty),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag("devices-empty"),
            )
        } else if (active.isNotEmpty()) {
            SettingsGroup(tag = "devices-active-group", title = null, showDivider = false) {
                active.forEach { device ->
                    DeviceRow(device, self, accountReadOnly, unnamed) { dialog = DeviceDialog(it, device.id) }
                }
            }
        }
        if (removed.isNotEmpty()) {
            SettingsGroup(tag = "devices-removed-group", title = null, showDivider = active.isNotEmpty()) {
                RemovedToggle(removed.size, removedExpanded) { removedExpanded = !removedExpanded }
                if (removedExpanded) {
                    removed.forEach { device ->
                        DeviceRow(device, self, accountReadOnly, unnamed) {}
                    }
                }
            }
        }
    }

    val open = dialog
    val target = open?.let { request -> state.devices.firstOrNull { it.id == request.deviceId } }
    if (open != null && target != null) {
        val close = { dialog = null }
        val displayName = target.name ?: unnamed
        when (open.action) {
            DeviceAction.REMOVE -> ConfirmDeviceDialog(
                confirmTag = "device-revoke-confirm-${target.id}",
                title = stringResource(R.string.peppy_devices_remove_title, displayName),
                paragraphs = removeParagraphs(target),
                confirmLabel = stringResource(R.string.peppy_devices_remove),
                failureText = stringResource(R.string.peppy_devices_remove_failed),
                busy = busy,
                onDismiss = close,
                onSubmit = { onResult -> submit(open) { device -> onResult(revoke(device)) } },
                onConfirmed = {
                    close()
                    onRefresh()
                },
            )
            DeviceAction.DISCONNECT -> ConfirmDeviceDialog(
                confirmTag = "device-revoke-confirm-${target.id}",
                title = stringResource(R.string.peppy_devices_disconnect_title),
                paragraphs = listOf(stringResource(R.string.peppy_devices_disconnect_body)),
                confirmLabel = stringResource(R.string.peppy_devices_disconnect),
                failureText = stringResource(R.string.peppy_devices_disconnect_failed),
                busy = busy,
                onDismiss = close,
                onSubmit = { onResult -> submit(open) { device -> onResult(revoke(device)) } },
                onConfirmed = {
                    close()
                    onChanged()
                },
            )
            DeviceAction.RENAME -> RenameDeviceDialog(
                device = target,
                busy = busy,
                onDismiss = close,
                onSave = { name, onResult -> submit(open) { device -> onResult(rename(device, name)) } },
                onRenamed = {
                    close()
                    onRefresh()
                },
            )
        }
    }
}

@Composable
private fun removeParagraphs(device: GatewayAccountDevice): List<String> = buildList {
    add(stringResource(R.string.peppy_devices_remove_body))
    if (device.role == "gateway") add(stringResource(R.string.peppy_devices_remove_gateway_note))
}

@Composable
private fun RemovedToggle(count: Int, expanded: Boolean, onToggle: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clickable(role = Role.Button, onClick = onToggle)
            .semantics {
                heading()
                if (expanded) collapse { onToggle(); true } else expand { onToggle(); true }
            }
            .testTag("devices-removed-toggle"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            stringResource(R.string.peppy_devices_previously_removed, count),
            Modifier.weight(1f),
            style = MaterialTheme.typography.titleMedium,
        )
        Icon(
            imageVector = if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun DeviceRow(
    device: GatewayAccountDevice,
    self: GatewayAccountDevice,
    accountReadOnly: Boolean,
    unnamed: String,
    onAction: (DeviceAction) -> Unit,
) {
    val displayName = device.name ?: unnamed
    val fontScale = LocalDensity.current.fontScale
    val resources = LocalResources.current
    val showRename = canRename(self, device)
    val showRemove = canRemove(self, device)
    val showDisconnect = canDisconnect(self, device)
    val kindLabel = kindLabelRes(device.kind)?.let { stringResource(it) }
    val badges = deviceBadges(device, self, accountReadOnly)
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 64.dp)
            .testTag("device-row-${device.id}"),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Row(
            Modifier.weight(1f).semantics(mergeDescendants = true) {},
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Icon(
                imageVector = kindIcon(device.kind),
                contentDescription = kindLabel,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
                Text(
                    displayName,
                    style = MaterialTheme.typography.bodyLarge,
                    maxLines = if (fontScale > 1.3f) 2 else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.testTag("device-name-${device.id}"),
                )
                Spacer(Modifier.height(4.dp))
                SingleLineBadges(
                    badges = badges,
                    overflowText = { resources.getString(R.string.peppy_devices_badges_more, it) },
                    tag = "device-badges-${device.id}",
                )
            }
        }
        if (!device.revoked && (showRename || showRemove || showDisconnect)) {
            DeviceMenu(device.id, displayName, showRename, showRemove, showDisconnect, onAction)
        } else {
            Spacer(Modifier.size(48.dp))
        }
    }
}

@Composable
private fun DeviceMenu(
    id: String,
    displayName: String,
    canRename: Boolean,
    canRemove: Boolean,
    canDisconnect: Boolean,
    onAction: (DeviceAction) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { expanded = true }, modifier = Modifier.testTag("device-menu-$id")) {
            Icon(
                Icons.Default.MoreVert,
                contentDescription = stringResource(R.string.peppy_devices_more_options, displayName),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            if (canRename) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.peppy_devices_rename)) },
                    onClick = {
                        expanded = false
                        onAction(DeviceAction.RENAME)
                    },
                    modifier = Modifier.testTag("device-rename-$id"),
                )
            }
            if (canRename && (canRemove || canDisconnect)) HorizontalDivider()
            if (canRemove) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.peppy_devices_remove), color = MaterialTheme.colorScheme.error) },
                    onClick = {
                        expanded = false
                        onAction(DeviceAction.REMOVE)
                    },
                    modifier = Modifier.testTag("device-remove-$id"),
                )
            }
            if (canDisconnect) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.peppy_devices_disconnect), color = MaterialTheme.colorScheme.error) },
                    onClick = {
                        expanded = false
                        onAction(DeviceAction.DISCONNECT)
                    },
                    modifier = Modifier.testTag("device-disconnect-$id"),
                )
            }
        }
    }
}

@Composable
private fun deviceBadges(device: GatewayAccountDevice, self: GatewayAccountDevice, accountReadOnly: Boolean): List<BadgeSpec> {
    val id = device.id
    val badges = mutableListOf<BadgeSpec>()
    if (device.id == self.id) {
        badges += BadgeSpec(stringResource(R.string.peppy_devices_this_device), "device-badge-self-$id")
    }
    if (accountReadOnly && device.role == "gateway" && !device.revoked) {
        badges += BadgeSpec(
            stringResource(R.string.peppy_devices_sending_paused_badge),
            "device-badge-paused-$id",
            BadgeTone.WARNING,
        )
    }
    if (!device.revoked) {
        val (badge, value) = presenceBadge(device.presence)
        val tone = if (badge == PresenceBadge.CONNECTED) BadgeTone.SUCCESS else BadgeTone.NEUTRAL
        badges += BadgeSpec(presenceLabel(badge, value), "device-badge-presence-$id", tone)
    }
    badges += BadgeSpec(roleLabel(device.role), "device-badge-role-$id")
    return badges
}

@Composable
private fun presenceLabel(badge: PresenceBadge, value: Long?): String = when (badge) {
    PresenceBadge.CONNECTED -> stringResource(R.string.peppy_devices_presence_connected)
    PresenceBadge.SEEN_NOW -> stringResource(R.string.peppy_devices_presence_seen_now)
    PresenceBadge.SEEN_MINUTES -> stringResource(R.string.peppy_devices_presence_seen_minutes, value ?: 0L)
    PresenceBadge.SEEN_HOURS -> stringResource(R.string.peppy_devices_presence_seen_hours, value ?: 0L)
    PresenceBadge.NEVER -> stringResource(R.string.peppy_devices_presence_never)
    PresenceBadge.UNKNOWN -> stringResource(R.string.peppy_devices_presence_unknown)
}

@Composable
private fun roleLabel(role: String): String = stringResource(
    when (role) {
        "owner" -> R.string.peppy_devices_role_owner
        "gateway" -> R.string.peppy_devices_role_gateway
        "device" -> R.string.peppy_devices_role_device
        else -> R.string.peppy_devices_role_unknown
    },
)

private fun kindIcon(kind: String?): ImageVector = when (kind) {
    "android" -> Icons.Default.PhoneAndroid
    "ios" -> Icons.Default.PhoneIphone
    "desktop" -> Icons.Default.Computer
    "browser" -> Icons.Default.Language
    else -> Icons.Default.Devices
}

private fun kindLabelRes(kind: String?): Int? = when (kind) {
    "android" -> R.string.peppy_devices_kind_android
    "ios" -> R.string.peppy_devices_kind_ios
    "desktop" -> R.string.peppy_devices_kind_desktop
    "browser" -> R.string.peppy_devices_kind_browser
    else -> null
}

@Composable
private fun ConfirmDeviceDialog(
    confirmTag: String,
    title: String,
    paragraphs: List<String>,
    confirmLabel: String,
    failureText: String,
    busy: Boolean,
    onDismiss: () -> Unit,
    onSubmit: ((Boolean) -> Unit) -> Unit,
    onConfirmed: () -> Unit,
) {
    var failed by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        modifier = Modifier.testTag("device-revoke-dialog"),
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                paragraphs.forEach { Text(it, style = MaterialTheme.typography.bodyMedium) }
                if (failed) {
                    Text(
                        failureText,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag("device-revoke-error").semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    failed = false
                    onSubmit { ok ->
                        if (ok) onConfirmed() else failed = true
                    }
                },
                enabled = !busy,
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                    contentColor = MaterialTheme.colorScheme.onError,
                ),
                modifier = Modifier.testTag(confirmTag),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (busy) {
                        CircularProgressIndicator(
                            Modifier.size(16.dp).testTag("device-revoke-progress"),
                            strokeWidth = 2.dp,
                        )
                        Spacer(Modifier.size(8.dp))
                    }
                    Text(confirmLabel)
                }
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy, modifier = Modifier.testTag("device-revoke-cancel")) {
                Text(stringResource(R.string.peppy_cancel))
            }
        },
    )
}

@Composable
private fun RenameDeviceDialog(
    device: GatewayAccountDevice,
    busy: Boolean,
    onDismiss: () -> Unit,
    onSave: (String, (RenameResult) -> Unit) -> Unit,
    onRenamed: () -> Unit,
) {
    val currentName = device.name.orEmpty()
    var value by remember { mutableStateOf(TextFieldValue(currentName, TextRange(0, currentName.length))) }
    var outcome by remember { mutableStateOf<RenameResult?>(null) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val trimmed = value.text.trim()
    val canSave = trimmed.isNotEmpty() && trimmed != currentName && !busy
    val invalid = outcome == RenameResult.InvalidName
    val failed = outcome == RenameResult.Failed

    fun save() {
        if (!canSave) return
        outcome = null
        onSave(trimmed) { result ->
            when (result) {
                RenameResult.Renamed -> onRenamed()
                RenameResult.InvalidName, RenameResult.Failed -> outcome = result
            }
        }
    }

    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        modifier = Modifier.testTag("device-rename-dialog"),
        title = { Text(stringResource(R.string.peppy_devices_rename_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedTextField(
                    value = value,
                    onValueChange = {
                        value = it
                        outcome = null
                    },
                    label = { Text(stringResource(R.string.peppy_devices_rename_field)) },
                    singleLine = true,
                    isError = invalid,
                    supportingText = if (invalid) {
                        {
                            Text(
                                stringResource(R.string.peppy_devices_rename_invalid),
                                modifier = Modifier.testTag("device-rename-invalid"),
                            )
                        }
                    } else {
                        null
                    },
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Words,
                        imeAction = ImeAction.Done,
                    ),
                    keyboardActions = KeyboardActions(onDone = { save() }),
                    modifier = Modifier.fillMaxWidth().focusRequester(focus).testTag("device-rename-field"),
                )
                if (failed) {
                    Text(
                        stringResource(R.string.peppy_devices_rename_failed),
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag("device-rename-error").semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { save() }, enabled = canSave, modifier = Modifier.testTag("device-rename-save")) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (busy) {
                        CircularProgressIndicator(
                            Modifier.size(16.dp).testTag("device-rename-progress"),
                            strokeWidth = 2.dp,
                        )
                        Spacer(Modifier.size(8.dp))
                    }
                    Text(stringResource(R.string.peppy_devices_rename_save))
                }
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy, modifier = Modifier.testTag("device-rename-cancel")) {
                Text(stringResource(R.string.peppy_cancel))
            }
        },
    )
}
