package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import android.text.format.DateUtils
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
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
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Contacts policy is durable only after explicit in-app consent and a successful readback. */
@Composable
internal fun ContactSyncSettings(context: Context, refresh: Int, onChanged: () -> Unit) {
    val scope = rememberCoroutineScope()
    var reload by remember { mutableIntStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var retireConfirm by remember { mutableStateOf(false) }
    var readConsent by remember { mutableStateOf(ConsentGatedToggle()) }
    var writeConsent by remember { mutableStateOf(ConsentGatedToggle()) }
    var selectedRemoteEdits by remember { mutableStateOf<String?>(null) }
    var readSettingsPending by remember { mutableStateOf(false) }
    var writeSettingsPending by remember { mutableStateOf(false) }
    var detailsExpanded by remember { mutableStateOf(false) }
    var actionError by remember { mutableStateOf<String?>(null) }
    val state by produceState<ContactSettingsState?>(null, refresh, reload) {
        value = withContext(Dispatchers.IO) { runCatching { ContactSyncHost.settingsState(context) }.getOrNull() }
    }

    fun reloadAfterAction() {
        busy = false
        reload++
        onChanged()
    }
    fun startAfterConsent() {
        busy = true
        scope.launch {
            try {
                val persisted = withContext(Dispatchers.IO) {
                    ContactSyncPreferences(context).enabled = true
                    ContactSyncPreferences(context).enabled
                }
                if (persisted && ContactSyncHost.access(context).read) {
                    withContext(Dispatchers.IO) {
                        ContactSyncHost.ensureObserver(context)
                        GatewayScheduler.schedule(context)
                    }
                } else actionError = context.getString(R.string.peppy_toggle_android_contacts_enable_error)
            } catch (_: Exception) {
                actionError = context.getString(R.string.peppy_toggle_android_contacts_enable_error)
            } finally {
                readSettingsPending = false
                readConsent = readConsent.cancel()
                reloadAfterAction()
            }
        }
    }
    fun applyRemoteEdits() {
        val mode = selectedRemoteEdits
        if (mode == null) {
            writeSettingsPending = false
            writeConsent = writeConsent.cancel()
            reloadAfterAction()
            return
        }
        busy = true
        scope.launch {
            try {
                val applied = withContext(Dispatchers.IO) {
                    if (mode != "off" && !ContactSyncHost.access(context).write) false
                    else ContactSyncHost.withCoordinator(context) { it.setRemoteEdits(mode) }
                }
                if (!applied) actionError = context.getString(R.string.peppy_toggle_android_contacts_remote_edits_error)
            } catch (_: Exception) {
                actionError = context.getString(R.string.peppy_toggle_android_contacts_remote_edits_error)
            } finally {
                selectedRemoteEdits = null
                writeSettingsPending = false
                writeConsent = writeConsent.cancel()
                reloadAfterAction()
            }
        }
    }
    fun act(block: () -> Unit) {
        busy = true
        scope.launch {
            withContext(Dispatchers.IO) { runCatching(block) }
            reloadAfterAction()
        }
    }
    fun consumeReadResult(granted: Boolean) {
        ContactSyncHost.onPermissionOrForeground(context)
        val transition = readConsent.accessResult(ContactSyncHost.access(context).read)
        readConsent = transition.state
        readSettingsPending = false
        if (transition.effect == ConsentEffect.APPLY) startAfterConsent() else reloadAfterAction()
    }
    fun consumeWriteResult(granted: Boolean) {
        ContactSyncHost.ensureObserver(context)
        val transition = writeConsent.accessResult(ContactSyncHost.access(context).write)
        writeConsent = transition.state
        writeSettingsPending = false
        if (transition.effect == ConsentEffect.APPLY) applyRemoteEdits() else {
            selectedRemoteEdits = null
            reloadAfterAction()
        }
    }
    fun openSettings(onIntent: (Intent) -> Unit) {
        runCatching {
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
        }.onSuccess(onIntent).onFailure { actionError = context.getString(R.string.peppy_toggle_android_settings_unavailable) }
    }
    fun consumeSettingsReturn(read: Boolean) {
        val granted = if (read) ContactSyncHost.access(context).read else ContactSyncHost.access(context).write
        if (read) {
            val transition = if (readSettingsPending && readConsent.phase == ConsentPhase.AWAITING_ACCESS) {
                readConsent.accessResult(granted)
            } else ConsentTransition(readConsent, ConsentEffect.NONE)
            readSettingsPending = false
            readConsent = transition.state
            if (transition.effect == ConsentEffect.APPLY) startAfterConsent() else reloadAfterAction()
        } else {
            val result = contactWriteSettingsResult(writeConsent, selectedRemoteEdits, granted, writeSettingsPending)
            writeSettingsPending = false
            writeConsent = result.state
            if (result.modeToApply != null) applyRemoteEdits() else {
                selectedRemoteEdits = null
                reloadAfterAction()
            }
        }
    }
    val readPermissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission(), ::consumeReadResult)
    val writePermissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission(), ::consumeWriteResult)
    val readSettingsLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { consumeSettingsReturn(read = true) }
    val writeSettingsLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { consumeSettingsReturn(read = false) }
    DisposableEffect(Unit) {
        onDispose {
            readConsent = readConsent.cancel()
            writeConsent = writeConsent.cancel()
            selectedRemoteEdits = null
            readSettingsPending = false
            writeSettingsPending = false
        }
    }

    val current = state
    Column(Modifier.testTag("contacts-tab-screen")) {
        HeroBanner(
            title = stringResource(R.string.peppy_toggle_android_contacts_hero_title),
            body = stringResource(R.string.peppy_toggle_android_contacts_hero_body),
            tag = "contacts-hero",
        )
        ScreenBody("contacts-screen-body") {
        Section("contacts-section", "Contact sync", showDivider = false) {
            val enabled = current?.enabled == true
            SettingToggleRow(
                title = stringResource(R.string.peppy_toggle_android_contacts_label),
                checked = enabled,
                enabled = current != null && !busy && !current.retirePending && readConsent.phase == ConsentPhase.IDLE && !retireConfirm,
                onCheckedChange = { requested ->
                    if (requested) readConsent = readConsent.requestEnable() else retireConfirm = true
                },
                tag = "contact-sync-row",
                subtitle = when {
                    current == null -> null
                    enabled && !current.readGranted -> stringResource(R.string.peppy_toggle_android_contacts_paused_hint)
                    !current.readGranted -> "Contacts access is needed to sync."
                    else -> null
                },
            )
            if (current != null) ContactStatus(current)
            if (retireConfirm) ContactRetireConfirmation(
                busy = busy,
                onCancel = { retireConfirm = false },
                onConfirm = { retireConfirm = false; act { ContactSyncHost.stopAndRetire(context) } },
            )
            actionError?.let { Text(it, Modifier.testTag("contact-action-error"), style = MaterialTheme.typography.bodySmall) }
            OutlinedButton({ detailsExpanded = !detailsExpanded }, Modifier.testTag("contact-details-row"), enabled = current != null && !busy) {
                Text(stringResource(if (detailsExpanded) R.string.peppy_toggle_android_contacts_hide_details else R.string.peppy_toggle_android_contacts_details))
            }
            if (current != null && detailsExpanded) ContactDetails(
                current = current,
                busy = busy,
                readSettingsPending = readSettingsPending,
                writeSettingsPending = writeSettingsPending || writeConsent.phase != ConsentPhase.IDLE,
                onOpenReadSettings = { openSettings { readSettingsLauncher.launch(it) } },
                onOpenWriteSettings = { openSettings { writeSettingsLauncher.launch(it) } },
                onSelectRemoteEdits = { mode ->
                    selectedRemoteEdits = mode
                    if (mode == "off") {
                        writeConsent = writeConsent.cancel()
                        writeSettingsPending = false
                        applyRemoteEdits()
                    } else if (writeConsent.phase != ConsentPhase.IDLE || writeSettingsPending) {
                        selectedRemoteEdits = null
                    } else if (ContactSyncHost.access(context).write) applyRemoteEdits()
                    else writeConsent = writeConsent.requestEnable()
                },
                act = ::act,
                context = context,
            )
        }
        }
    }

    if (readConsent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_contacts_consent_title),
        body = stringResource(R.string.peppy_contacts_description),
        tag = "contacts-consent-sheet",
        disclosureTag = "contact-sync-description",
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = stringResource(R.string.peppy_toggle_android_contacts_remote_edit_next),
        onDismiss = { readConsent = readConsent.cancel() },
        onConfirm = {
            val transition = readConsent.agree(current?.readGranted == true)
            readConsent = transition.state
            when (transition.effect) {
                ConsentEffect.APPLY -> startAfterConsent()
                ConsentEffect.REQUEST_ACCESS -> if (current?.readPermissionRequested == true) {
                    readSettingsPending = true
                    openSettings { readSettingsLauncher.launch(it) }
                } else {
                    ContactSyncPreferences(context).readPermissionRequested = true
                    readPermissionLauncher.launch(Manifest.permission.READ_CONTACTS)
                }
                ConsentEffect.NONE -> Unit
            }
        },
    )
    if (writeConsent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
        title = stringResource(R.string.peppy_toggle_android_write_consent_title),
        body = stringResource(R.string.peppy_toggle_android_contacts_write_consent_body),
        tag = "contacts-write-consent-sheet",
        confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
        nextStep = stringResource(R.string.peppy_toggle_android_contacts_write_next),
        onDismiss = { writeConsent = writeConsent.cancel(); selectedRemoteEdits = null },
        onConfirm = {
            val transition = writeConsent.agree(current?.writeGranted == true)
            writeConsent = transition.state
            when (transition.effect) {
                ConsentEffect.APPLY -> applyRemoteEdits()
                ConsentEffect.REQUEST_ACCESS -> if (ContactSyncPreferences(context).writePermissionRequested) {
                    writeSettingsPending = true
                    openSettings { writeSettingsLauncher.launch(it) }
                } else {
                    ContactSyncPreferences(context).writePermissionRequested = true
                    writePermissionLauncher.launch(Manifest.permission.WRITE_CONTACTS)
                }
                ConsentEffect.NONE -> Unit
            }
        },
    )
}

@Composable
private fun ContactRetireConfirmation(busy: Boolean, onCancel: () -> Unit, onConfirm: () -> Unit) {
    Column(Modifier.testTag("contact-retire-confirmation"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.peppy_toggle_android_contacts_stop_retire_consequences), style = MaterialTheme.typography.bodySmall)
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onCancel, Modifier.testTag("contact-retire-cancel"), enabled = !busy) { Text("Keep syncing") }
            Button(onConfirm, Modifier.testTag("contact-retire-confirm"), enabled = !busy) { Text("Stop and retire") }
        }
    }
}

@Composable
private fun ContactDetails(
    current: ContactSettingsState,
    busy: Boolean,
    readSettingsPending: Boolean,
    writeSettingsPending: Boolean,
    onOpenReadSettings: () -> Unit,
    onOpenWriteSettings: () -> Unit,
    onSelectRemoteEdits: (String) -> Unit,
    act: (() -> Unit) -> Unit,
    context: Context,
) {
    StatusRow(
        "contact-access-status",
        "Contacts access",
        when {
            current.readGranted && current.writeGranted -> stringResource(R.string.peppy_contacts_access_read_write)
            current.readGranted -> stringResource(R.string.peppy_contacts_access_read_only)
            else -> stringResource(R.string.peppy_contacts_access_not_allowed)
        },
    )
    if (!current.readGranted) {
        Text(stringResource(R.string.peppy_contacts_access_denied), Modifier.testTag("contact-denied-status"), style = MaterialTheme.typography.bodySmall)
        if (readSettingsPending) OutlinedButton(onOpenReadSettings, Modifier.testTag("contact-open-settings-button"), enabled = !busy) { Text(stringResource(R.string.peppy_contacts_open_settings)) }
    }
    if (!current.writeGranted) Text(
        stringResource(R.string.peppy_contacts_allow_edits_footer),
        Modifier.testTag("contact-allow-edits-footer"),
        style = MaterialTheme.typography.bodySmall,
    )
    val overview = current.overview ?: return
    overview.contactCount?.let { StatusRow("contact-sync-count", stringResource(R.string.peppy_contacts_read_label), it.toString()) }
    val writable = overview.accounts.filter { it.writable }
    if (writable.isNotEmpty()) {
        Text("Save new contacts to", style = MaterialTheme.typography.labelMedium)
        Column(Modifier.testTag("contact-account-list"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            writable.forEach { account ->
                val selected = overview.defaultAccountId == account.id
                OutlinedButton(
                    { act { ContactSyncHost.withCoordinator(context) { it.setDefaultAccount(account.id) } } },
                    Modifier.semantics { this.selected = selected }.testTag("contact-account-${account.id}"),
                    enabled = !busy,
                ) { Text((if (selected) "✓ " else "") + account.name) }
            }
        }
    }
    Text(stringResource(R.string.peppy_toggle_android_contacts_edits_label), Modifier.testTag("remote-edits-row"), style = MaterialTheme.typography.labelMedium)
    if (writeSettingsPending && !current.writeGranted) {
        OutlinedButton(onOpenWriteSettings, Modifier.testTag("contact-write-open-settings-button"), enabled = !busy) {
            Text(stringResource(R.string.peppy_contacts_open_settings))
        }
    }
    Text(
        when (overview.remoteEdits) {
            "auto" -> stringResource(R.string.peppy_toggle_android_contacts_auto_policy)
            "off" -> stringResource(R.string.peppy_toggle_android_contacts_off_policy)
            else -> stringResource(R.string.peppy_toggle_android_contacts_confirm_policy)
        },
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.testTag("remote-edits-policy-description"),
    )
    Column(Modifier.testTag("remote-edits-policy"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        listOf("off" to stringResource(R.string.peppy_toggle_android_contacts_off_label), "confirm" to stringResource(R.string.peppy_toggle_android_contacts_confirm_label), "auto" to stringResource(R.string.peppy_toggle_android_contacts_auto_label)).forEach { (mode, label) ->
            OutlinedButton(
                { onSelectRemoteEdits(mode) },
                Modifier.semantics { selected = overview.remoteEdits == mode }.testTag("remote-edits-$mode"),
                enabled = !busy && !writeSettingsPending,
            ) { Text((if (overview.remoteEdits == mode) "✓ " else "") + label) }
        }
    }
    if (overview.pending.isNotEmpty()) Column(Modifier.testTag("contact-pending-list"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        overview.pending.forEach { PendingRow(it, busy, context, act) }
    }
}

@Composable
private fun ContactStatus(current: ContactSettingsState) {
    when {
        current.retirePending -> Text("Retirement will be published the next time the vault is unlocked.", Modifier.testTag("contact-retire-pending"), style = MaterialTheme.typography.bodySmall)
        current.stopped -> Text(stringResource(R.string.peppy_contacts_stopped), Modifier.testTag("contact-stopped-status"), style = MaterialTheme.typography.bodySmall)
        current.enabled && !current.unlocked -> Text("Unlock the vault above to sync contacts.", Modifier.testTag("contact-vault-status"), style = MaterialTheme.typography.bodySmall)
        current.enabled && current.needsUnlock -> Text("Unlock with your passphrase to finish preparing contact sync.", Modifier.testTag("contact-server-status"), style = MaterialTheme.typography.bodySmall)
        current.enabled && current.backfillPending -> Text("Preparing existing sync history for contacts. This continues in the background.", Modifier.testTag("contact-server-status"), style = MaterialTheme.typography.bodySmall)
        current.enabled && !current.contactsReady -> Text("Waiting for a compatible server. Contacts sync only after the server supports compaction.", Modifier.testTag("contact-server-status"), style = MaterialTheme.typography.bodySmall)
        current.enabled && current.inactiveRoster -> Text("Some devices have not upgraded yet. Contact sync is available; history retention is paused.", Modifier.testTag("contact-server-status"), style = MaterialTheme.typography.bodySmall)
    }
    current.overview?.let { overview ->
        if (overview.bookId == null && current.enabled) Text("Waiting for the first contact scan.", Modifier.testTag("contact-sync-status"), style = MaterialTheme.typography.bodySmall)
        else if (overview.bookId != null) StatusRow("contact-sync-status", "Last full scan", overview.lastFullScanAt?.let { DateUtils.getRelativeTimeSpanString(it).toString() } ?: "Not yet")
        if (overview.pending.isNotEmpty()) Text(
            if (overview.pending.size == 1) stringResource(R.string.peppy_toggle_android_contacts_pending_summary_one)
            else stringResource(R.string.peppy_toggle_android_contacts_pending_summary_many, overview.pending.size),
            Modifier.testTag("contact-pending-summary"),
            style = MaterialTheme.typography.bodySmall,
        )
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
            if (item.state == "outcome_unknown") OutlinedButton({ act { ContactSyncHost.withCoordinator(context) { it.resolveUnknown(item.id) } } }, Modifier.testTag("$tag-resolve"), enabled = !busy) { Text("Dismiss") }
            else if (item.isHold || item.state in setOf("requested", "awaiting_approval")) {
                Button({ act { ContactSyncHost.withCoordinator(context) { it.decide(item, approve = true) } } }, Modifier.testTag("$tag-approve"), enabled = !busy) { Text("Approve") }
                OutlinedButton({ act { ContactSyncHost.withCoordinator(context) { it.decide(item, approve = false) } } }, Modifier.testTag("$tag-reject"), enabled = !busy) { Text("Reject") }
            }
        }
    }
}
