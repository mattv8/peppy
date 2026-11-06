package dev.peppy.mobile.contacts

import android.Manifest
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
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
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.GatewayScheduler
import dev.peppy.mobile.R
import dev.peppy.mobile.enrollment.ConsentEffect
import dev.peppy.mobile.enrollment.ConsentGatedToggle
import dev.peppy.mobile.enrollment.ConsentPhase
import dev.peppy.mobile.enrollment.ConsentTransition
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.DetailsNavigationRow
import dev.peppy.mobile.ui.HeroBanner
import dev.peppy.mobile.ui.ScreenBody
import dev.peppy.mobile.ui.SettingToggleRow
import dev.peppy.mobile.ui.theme.PeppyTokens
import androidx.compose.foundation.isSystemInDarkTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Contacts policy is durable only after explicit in-app consent and a successful readback. */
@Composable
internal fun ContactSyncSettings(context: Context, refresh: Int, onChanged: () -> Unit, onDetailsChanged: (Boolean) -> Unit = {}) {
    val scope = rememberCoroutineScope()
    var reload by remember { mutableIntStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var retireConfirm by remember { mutableStateOf(false) }
    var readConsent by remember { mutableStateOf(ConsentGatedToggle()) }
    var writeConsent by remember { mutableStateOf(ConsentGatedToggle()) }
    var selectedRemoteEdits by remember { mutableStateOf<String?>(null) }
    var readSettingsPending by remember { mutableStateOf(false) }
    var writeSettingsPending by remember { mutableStateOf(false) }
    var detailsOpen by remember { mutableStateOf(false) }
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
    fun act(block: () -> Boolean) {
        busy = true
        scope.launch {
            try {
                if (!withContext(Dispatchers.IO) { block() }) {
                    actionError = "Couldn't save this change. Try again."
                }
            } catch (exception: CancellationException) {
                throw exception
            } catch (_: Exception) {
                actionError = "Couldn't save this change. Try again."
            } finally {
                reloadAfterAction()
            }
        }
    }
    fun selectRemoteEdits(mode: String) {
        selectedRemoteEdits = mode
        if (mode == "off") {
            writeConsent = writeConsent.cancel()
            writeSettingsPending = false
            applyRemoteEdits()
        } else if (writeConsent.phase != ConsentPhase.IDLE || writeSettingsPending) {
            selectedRemoteEdits = null
        } else if (ContactSyncHost.access(context).write) applyRemoteEdits()
        else writeConsent = writeConsent.requestEnable()
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
    DisposableEffect(detailsOpen) {
        onDetailsChanged(detailsOpen)
        onDispose { onDetailsChanged(false) }
    }

    val current = state
    Column(Modifier.testTag("contacts-tab-screen")) {
        if (!detailsOpen || current == null) HeroBanner(
            title = stringResource(R.string.peppy_toggle_android_contacts_hero_title),
            body = stringResource(R.string.peppy_toggle_android_contacts_hero_body),
            tag = "contacts-hero",
        )
        ScreenBody(if (detailsOpen && current != null) "contacts-details-screen" else "contacts-screen-body") {
        if (detailsOpen && current != null) {
            ContactSyncDetailsContent(
                current = current,
                busy = busy,
                readSettingsPending = readSettingsPending,
                writeSettingsPending = writeSettingsPending || writeConsent.phase != ConsentPhase.IDLE,
                onBack = { detailsOpen = false },
                onOpenReadSettings = { openSettings { readSettingsLauncher.launch(it) } },
                onOpenWriteSettings = { openSettings { writeSettingsLauncher.launch(it) } },
                onSelectRemoteEdits = ::selectRemoteEdits,
                onSelectAccount = { id -> act { ContactSyncHost.withCoordinator(context) { it.setDefaultAccount(id) } } },
                onDecide = { item, approve -> act { ContactSyncHost.withCoordinator(context) { it.decide(item, approve = approve) } } },
                onResolveUnknown = { item -> act { ContactSyncHost.withCoordinator(context) { it.resolveUnknown(item.id) } } },
                actionError = actionError,
            )
        } else Column(Modifier.fillMaxWidth().testTag("contacts-section"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
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
                    enabled && current.readGranted && current.overview?.lastFullScanAt != null -> stringResource(
                        R.string.peppy_toggle_android_contacts_on_last_checked,
                        current.overview.lastFullScanAt.let { android.text.format.DateUtils.getRelativeTimeSpanString(it).toString() },
                    )
                    enabled && !current.readGranted -> stringResource(R.string.peppy_toggle_android_contacts_paused_hint)
                    !current.readGranted -> "Contacts access is needed to sync."
                    else -> null
                },
            )
            if (current != null) ContactStatus(current)
            current?.overview?.pending?.takeIf { it.isNotEmpty() }?.let { pending ->
                val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
                Surface(
                    Modifier.fillMaxWidth().testTag("contact-pending-summary"),
                    shape = RoundedCornerShape(12.dp),
                    color = colors.warningSurface,
                    contentColor = colors.warningText,
                ) {
                    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            if (pending.size == 1) stringResource(R.string.peppy_toggle_android_contacts_pending_summary_one)
                            else stringResource(R.string.peppy_toggle_android_contacts_pending_summary_many, pending.size),
                            style = MaterialTheme.typography.bodySmall,
                        )
                        OutlinedButton({ detailsOpen = true }, Modifier.heightIn(min = 48.dp).testTag("contact-pending-review")) {
                            Text(stringResource(R.string.peppy_toggle_android_contacts_review))
                        }
                    }
                }
            }
            if (retireConfirm) ContactRetireConfirmation(
                busy = busy,
                onCancel = { retireConfirm = false },
                onConfirm = { retireConfirm = false; act { ContactSyncHost.stopAndRetire(context); true } },
            )
            actionError?.let { Text(it, Modifier.testTag("contact-action-error"), style = MaterialTheme.typography.bodySmall) }
            DetailsNavigationRow("contact-details-row", stringResource(R.string.peppy_toggle_android_contacts_details), null) {
                if (current != null && !busy) detailsOpen = true
            }
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
    }
}
