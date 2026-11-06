package dev.peppy.mobile.contacts

import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.DetailsPageHeader
import dev.peppy.mobile.ui.DetailsStatusRow as StatusRow

internal fun showAccountPicker(writableAccounts: Int): Boolean = writableAccounts >= 2

@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
internal fun ContactSyncDetailsContent(
    current: ContactSettingsState,
    busy: Boolean,
    readSettingsPending: Boolean,
    writeSettingsPending: Boolean,
    onBack: () -> Unit,
    onOpenReadSettings: () -> Unit,
    onOpenWriteSettings: () -> Unit,
    onSelectRemoteEdits: (String) -> Unit,
    onSelectAccount: (String) -> Unit,
    onDecide: (ContactPendingItem, Boolean) -> Unit,
    onResolveUnknown: (ContactPendingItem) -> Unit,
    actionError: String? = null,
) {
    val overview = current.overview
    val remoteEdits = overview?.remoteEdits ?: "confirm"
    val modes = listOf(
        "off" to stringResource(R.string.peppy_toggle_android_contacts_off_label),
        "confirm" to stringResource(R.string.peppy_toggle_android_contacts_confirm_label),
        "auto" to stringResource(R.string.peppy_toggle_android_contacts_auto_label),
    )
    Column(
        Modifier.fillMaxWidth().testTag("contacts-details-content"),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        DetailsPageHeader(stringResource(R.string.peppy_toggle_android_contacts_details), "contacts-details", onBack)
        if (overview?.pending?.isNotEmpty() == true) {
            Text(
                stringResource(R.string.peppy_toggle_android_contacts_pending_title),
                Modifier.semantics { heading() },
                style = MaterialTheme.typography.titleMedium,
            )
            Column(Modifier.testTag("contact-pending-list"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                overview.pending.forEach { PendingRow(it, busy, onDecide, onResolveUnknown) }
            }
        }
        ContactAccess(current, busy, readSettingsPending, onOpenReadSettings)
        overview?.contactCount?.let { StatusRow("contact-sync-count", stringResource(R.string.peppy_contacts_read_label), it.toString()) }
        if (overview?.bookId != null) {
            StatusRow(
                "contact-sync-status",
                stringResource(R.string.peppy_toggle_android_contacts_last_checked),
                overview.lastFullScanAt?.let { DateUtils.getRelativeTimeSpanString(it).toString() } ?: "Not yet",
            )
        } else if (current.enabled) {
            Text("Waiting for the first contact scan.", Modifier.testTag("contact-sync-status"), style = MaterialTheme.typography.bodySmall)
        }
        Text(stringResource(R.string.peppy_toggle_android_contacts_edits_label), Modifier.testTag("remote-edits-row"), style = MaterialTheme.typography.titleSmall)
        if (writeSettingsPending && !current.writeGranted) {
            OutlinedButton(onOpenWriteSettings, Modifier.testTag("contact-write-open-settings-button"), enabled = !busy) {
                Text(stringResource(R.string.peppy_contacts_open_settings))
            }
        }
        RemoteEditsPolicy(modes, remoteEdits, busy, writeSettingsPending, onSelectRemoteEdits)
        Text(
            policyDescription(remoteEdits),
            Modifier.testTag("remote-edits-policy-description"),
            style = MaterialTheme.typography.bodySmall,
        )
        overview?.let { ContactAccountPicker(it, busy, onSelectAccount) }
        actionError?.let { Text(it, Modifier.testTag("contact-action-error"), style = MaterialTheme.typography.bodySmall) }
    }
}

@Composable
private fun ContactAccess(current: ContactSettingsState, busy: Boolean, readSettingsPending: Boolean, onOpenReadSettings: () -> Unit) {
    StatusRow("contact-access-status", "Contacts access", when {
        current.readGranted && current.writeGranted -> stringResource(R.string.peppy_contacts_access_read_write)
        current.readGranted -> stringResource(R.string.peppy_contacts_access_read_only)
        else -> stringResource(R.string.peppy_contacts_access_not_allowed)
    })
    if (!current.readGranted) {
        Text(stringResource(R.string.peppy_contacts_access_denied), Modifier.testTag("contact-denied-status"), style = MaterialTheme.typography.bodySmall)
        if (readSettingsPending) OutlinedButton(onOpenReadSettings, Modifier.testTag("contact-open-settings-button"), enabled = !busy) {
            Text(stringResource(R.string.peppy_contacts_open_settings))
        }
    }
    if (!current.writeGranted) Text(
        stringResource(R.string.peppy_contacts_allow_edits_footer),
        Modifier.testTag("contact-allow-edits-footer"),
        style = MaterialTheme.typography.bodySmall,
    )
}

@Composable
private fun RemoteEditsPolicy(
    modes: List<Pair<String, String>>,
    selectedMode: String,
    busy: Boolean,
    writeSettingsPending: Boolean,
    onSelectRemoteEdits: (String) -> Unit,
) {
    val enabled = !busy && !writeSettingsPending
    BoxWithConstraints {
        val useRadioButtons = LocalDensity.current.fontScale > 1.3f || maxWidth < 400.dp
        if (useRadioButtons) {
        Column(Modifier.testTag("remote-edits-policy")) {
            modes.forEach { (mode, label) ->
                Row(
                    Modifier.fillMaxWidth().heightIn(min = 48.dp).selectable(
                        selected = selectedMode == mode,
                        enabled = enabled,
                        role = Role.RadioButton,
                        onClick = { onSelectRemoteEdits(mode) },
                    ).testTag("remote-edits-$mode"),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selectedMode == mode, onClick = null, enabled = enabled)
                    Text(label, Modifier.padding(start = 8.dp))
                }
            }
        }
        } else {
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().testTag("remote-edits-policy")) {
            modes.forEachIndexed { index, (mode, label) ->
                SegmentedButton(
                    selected = selectedMode == mode,
                    onClick = { onSelectRemoteEdits(mode) },
                    shape = SegmentedButtonDefaults.itemShape(index, modes.size),
                    enabled = enabled,
                    modifier = Modifier.testTag("remote-edits-$mode"),
                    label = { Text(label) },
                )
            }
        }
        }
    }
}

@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
private fun ContactAccountPicker(overview: ContactOverview, busy: Boolean, onSelectAccount: (String) -> Unit) {
    val writable = overview.accounts.filter { it.writable }
    if (!showAccountPicker(writable.size)) return
    var expanded by remember { mutableStateOf(false) }
    val selected = writable.firstOrNull { it.id == overview.defaultAccountId } ?: writable.first()
    ExposedDropdownMenuBox(expanded, { expanded = !expanded }, Modifier.testTag("contact-account-list")) {
        OutlinedTextField(
            value = selected.name,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.peppy_toggle_android_contacts_save_to)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth(),
            enabled = !busy,
        )
        ExposedDropdownMenu(expanded, { expanded = false }) {
            writable.forEach { account ->
                DropdownMenuItem(
                    text = { Text(account.name) },
                    onClick = { expanded = false; onSelectAccount(account.id) },
                    modifier = Modifier.testTag("contact-account-${account.id}"),
                )
            }
        }
    }
}

@Composable
private fun PendingRow(item: ContactPendingItem, busy: Boolean, onDecide: (ContactPendingItem, Boolean) -> Unit, onResolveUnknown: (ContactPendingItem) -> Unit) {
    val tag = if (item.isHold) "contact-hold-${item.id}" else "contact-request-${item.id}"
    Column(Modifier.fillMaxWidth().testTag(tag), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        val description = when {
            item.isHold -> "${item.count ?: 0} contacts disappeared from this phone. Remove them from your other devices?"
            item.state == "outcome_unknown" -> "This phone could not confirm a desktop ${item.kind} of ${item.displayName ?: "a contact"}. Check the contact, then dismiss."
            else -> "Desktop ${item.kind}: ${item.displayName ?: "contact"}" + item.fieldPaths.takeIf { it.isNotEmpty() }?.let { " (${it.joinToString()})" }.orEmpty()
        }
        Text(description, style = MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (item.state == "outcome_unknown") OutlinedButton({ onResolveUnknown(item) }, Modifier.testTag("$tag-resolve"), enabled = !busy) { Text("Dismiss") }
            else if (item.isHold || item.state in setOf("requested", "awaiting_approval")) {
                androidx.compose.material3.Button({ onDecide(item, true) }, Modifier.testTag("$tag-approve"), enabled = !busy) { Text("Approve") }
                OutlinedButton({ onDecide(item, false) }, Modifier.testTag("$tag-reject"), enabled = !busy) { Text("Reject") }
            }
        }
    }
}

@Composable
private fun policyDescription(mode: String): String = when (mode) {
    "auto" -> stringResource(R.string.peppy_toggle_android_contacts_auto_policy)
    "off" -> stringResource(R.string.peppy_toggle_android_contacts_off_policy)
    else -> stringResource(R.string.peppy_toggle_android_contacts_confirm_policy)
}
