package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal val CONTACT_PERMISSIONS = arrayOf(Manifest.permission.READ_CONTACTS)

/**
 * Contact sync settings. Consent is local; everything else (book state, policy, default account,
 * pending approvals, uncertain writes, last full scan) is read from and written to the core.
 * Nothing here scans or writes OS contacts.
 */
@Composable
internal fun ContactSyncSettings(
    context: Context,
    refresh: Int,
    requestReadPermission: () -> Unit,
    requestWritePermission: () -> Unit,
    openAppSettings: () -> Unit,
    onChanged: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var reload by remember { mutableIntStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var retireConfirm by remember { mutableStateOf(false) }
    val state by produceState<ContactSettingsState?>(null, refresh, reload) {
        value = withContext(Dispatchers.IO) { runCatching { ContactSyncHost.settingsState(context) }.getOrNull() }
    }
    fun act(block: () -> Unit) {
        busy = true
        scope.launch {
            withContext(Dispatchers.IO) { runCatching(block) }
            busy = false
            reload++
            onChanged()
        }
    }
    fun startAndRequestRead() {
        busy = true
        scope.launch {
            withContext(Dispatchers.IO) { ContactSyncPreferences(context).startAfterReadGrant = true }
            requestReadPermission()
            busy = false
            reload++
            onChanged()
        }
    }
    fun startAndOpenSettings() {
        busy = true
        scope.launch {
            withContext(Dispatchers.IO) { ContactSyncPreferences(context).startAfterReadGrant = true }
            openAppSettings()
            busy = false
        }
    }

    Section("contact-sync-section", "Contact sync", showDivider = false) {
        val current = state
        val enabled = current?.enabled == true
        Text(
            stringResource(R.string.peppy_contacts_description),
            style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-sync-description"),
        )
        if (current == null) return@Section
        if (!enabled) {
            if (current.stopped) {
                Text(stringResource(R.string.peppy_contacts_stopped), style = MaterialTheme.typography.bodySmall)
            }
            if (current.retirePending) {
                Text("Retirement will be published the next time the vault is unlocked.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-retire-pending"))
            } else if (current.readGranted) {
                Button({ act {
                    ContactSyncPreferences(context).enabled = true
                    ContactSyncHost.ensureObserver(context)
                    GatewayScheduler.schedule(context)
                } }, Modifier.testTag("contact-start-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_start)) }
            } else if (current.stopped) {
                if (current.readPermissionRequested) {
                    Button(::startAndOpenSettings, Modifier.testTag("contact-open-settings-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_open_settings)) }
                } else {
                    Button(::startAndRequestRead, Modifier.testTag("contact-start-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_start)) }
                }
            } else {
                if (current.readPermissionRequested) {
                    Text(stringResource(R.string.peppy_contacts_access_denied), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-denied-status"))
                    Button(openAppSettings, Modifier.testTag("contact-open-settings-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_open_settings)) }
                } else {
                    Button(requestReadPermission, Modifier.testTag("contact-allow-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_allow_access)) }
                }
                Text(stringResource(R.string.peppy_contacts_start_explainer), style = MaterialTheme.typography.bodySmall)
            }
            current.overview?.pending?.takeIf { it.isNotEmpty() }?.let { pending ->
                Column(Modifier.testTag("contact-pending-list"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    pending.forEach { item -> PendingRow(item, busy, context, ::act) }
                }
            }
            return@Section
        }
        StatusRow(
            "contact-access-status", "Contacts access",
            when {
                current.readGranted && current.writeGranted -> stringResource(R.string.peppy_contacts_access_read_write)
                current.readGranted -> stringResource(R.string.peppy_contacts_access_read_only)
                else -> stringResource(R.string.peppy_contacts_access_not_allowed)
            },
        )
        if (!current.readGranted) {
            Text(stringResource(R.string.peppy_contacts_access_denied), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-denied-status"))
            if (current.readPermissionRequested) {
                OutlinedButton(openAppSettings, Modifier.testTag("contact-open-settings-button")) { Text(stringResource(R.string.peppy_contacts_open_settings)) }
            } else {
                OutlinedButton(requestReadPermission, Modifier.testTag("contact-allow-button")) { Text(stringResource(R.string.peppy_contacts_allow_access)) }
            }
        } else if (!current.writeGranted) {
            OutlinedButton(requestWritePermission, Modifier.testTag("contact-grant-write-button")) { Text(stringResource(R.string.peppy_contacts_allow_edits)) }
            Text(stringResource(R.string.peppy_contacts_allow_edits_footer), style = MaterialTheme.typography.bodySmall)
        }
        if (enabled && !current.unlocked) {
            Text("Unlock the vault above to sync contacts.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-vault-status"))
        } else if (enabled && current.needsUnlock) {
            Text(
                "Unlock with your passphrase to finish preparing contact sync.",
                style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-server-status"),
            )
        } else if (enabled && current.backfillPending) {
            Text(
                "Preparing existing sync history for contacts. This continues in the background.",
                style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-server-status"),
            )
        } else if (enabled && !current.contactsReady) {
            Text(
                "Waiting for a compatible server. Contacts sync only after the server supports compaction.",
                style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-server-status"),
            )
        } else if (enabled && current.inactiveRoster) {
            Text(
                "Some devices have not upgraded yet. Contact sync is available; history retention is paused.",
                style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-server-status"),
            )
        }
        if (current.retirePending) {
            Text("Retirement will be published the next time the vault is unlocked.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-retire-pending"))
        }
        val overview = current.overview ?: return@Section
        if (overview.bookId == null) {
            if (enabled) Text("Waiting for the first contact scan.", style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("contact-sync-status"))
            return@Section
        }
        StatusRow(
            "contact-sync-status", "Last full scan",
            overview.lastFullScanAt?.let { DateUtils.getRelativeTimeSpanString(it).toString() } ?: "Not yet",
        )
        overview.contactCount?.let { StatusRow("contact-sync-count", stringResource(R.string.peppy_contacts_read_label), it.toString()) }

        val writable = overview.accounts.filter { it.writable }
        if (writable.isNotEmpty()) {
            Text("Save new contacts to", style = MaterialTheme.typography.labelMedium)
            Column(Modifier.testTag("contact-account-list"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                writable.forEach { account ->
                    val isSelected = overview.defaultAccountId == account.id
                    OutlinedButton(
                        { act { ContactSyncHost.withCoordinator(context) { it.setDefaultAccount(account.id) } } },
                        Modifier.semantics { selected = isSelected }.testTag("contact-account-${account.id}"),
                        enabled = !busy,
                    ) { Text((if (isSelected) "✓ " else "") + account.name) }
                }
            }
        }

        Text("Desktop edits", style = MaterialTheme.typography.labelMedium)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.testTag("remote-edits-policy")) {
            listOf("auto" to "Auto", "confirm" to "Confirm", "off" to "Off").forEach { (mode, label) ->
                val isSelected = overview.remoteEdits == mode
                OutlinedButton(
                    { act { ContactSyncHost.withCoordinator(context) { it.setRemoteEdits(mode) } } },
                    Modifier.semantics { selected = isSelected }.testTag("remote-edits-$mode"),
                    enabled = !busy && current.writeGranted,
                ) { Text((if (isSelected) "✓ " else "") + label) }
            }
        }
        Text(
            if (!current.writeGranted) stringResource(R.string.peppy_contacts_access_read_only) else when (overview.remoteEdits) {
                "auto" -> "Small changes apply automatically. Deleting more than 10 contacts or 5% in 24 hours asks for approval here."
                "off" -> "Desktop edits are not accepted."
                else -> "Every desktop edit waits for approval on this phone."
            },
            style = MaterialTheme.typography.bodySmall,
        )

        if (overview.pending.isNotEmpty()) {
            Text("Pending on this phone", style = MaterialTheme.typography.labelMedium)
            Column(Modifier.testTag("contact-pending-list"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                overview.pending.forEach { item -> PendingRow(item, busy, context, ::act) }
            }
        }

        if (!retireConfirm) {
            OutlinedButton({ retireConfirm = true }, Modifier.testTag("contact-retire-button"), enabled = !busy) { Text("Stop syncing") }
        } else {
            Column(Modifier.testTag("contact-retire-confirmation"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "This stops contact sync and tells your other devices this book is retired. Contacts on this phone are not changed or deleted.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton({ retireConfirm = false }, Modifier.testTag("contact-retire-cancel")) { Text("Keep syncing") }
                    Button({ retireConfirm = false; act { ContactSyncHost.stopAndRetire(context) } }, Modifier.testTag("contact-retire-confirm"), enabled = !busy) {
                        Text("Stop and retire")
                    }
                }
            }
        }
    }
}

@Composable
private fun PendingRow(item: ContactPendingItem, busy: Boolean, context: Context, act: (() -> Unit) -> Unit) {
    val tag = if (item.isHold) "contact-hold-${item.id}" else "contact-request-${item.id}"
    Column(Modifier.fillMaxWidth().testTag(tag), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        val description = when {
            item.isHold -> "${item.count ?: 0} contacts disappeared from this phone. Remove them from your other devices?"
            item.state == "outcome_unknown" -> "This phone could not confirm a desktop ${item.kind} of ${item.displayName ?: "a contact"}. Check the contact, then dismiss."
            else -> "Desktop ${item.kind}: ${item.displayName ?: "contact"}" + item.fieldPaths.takeIf { it.isNotEmpty() }?.let { " (${it.joinToString()})" }.orEmpty()
        }
        Text(description, style = MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (item.state == "outcome_unknown") {
                OutlinedButton({ act { ContactSyncHost.withCoordinator(context) { it.resolveUnknown(item.id) } } }, Modifier.testTag("$tag-resolve"), enabled = !busy) {
                    Text("Dismiss")
                }
            } else if (item.isHold || item.state in setOf("requested", "awaiting_approval")) {
                Button({ act { ContactSyncHost.withCoordinator(context) { it.decide(item, approve = true) } } }, Modifier.testTag("$tag-approve"), enabled = !busy) {
                    Text("Approve")
                }
                OutlinedButton({ act { ContactSyncHost.withCoordinator(context) { it.decide(item, approve = false) } } }, Modifier.testTag("$tag-reject"), enabled = !busy) {
                    Text("Reject")
                }
            }
        }
    }
}
