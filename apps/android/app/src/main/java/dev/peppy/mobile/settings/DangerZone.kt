package dev.peppy.mobile.settings

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.account.HostedAccountDeletionRow
import dev.peppy.mobile.billing.PlayDeletionContext
import dev.peppy.mobile.ui.DestructiveRow
import dev.peppy.mobile.ui.SettingsGroup
import dev.peppy.mobile.ui.theme.PeppyTokens

private const val VAULT_ERASE_CONFIRMATION = "ERASE"

/**
 * The vault delete runs in SettingsScreen, which outlives eligibility changes. This group only
 * renders state and submits; the dialog closes once the in-flight delete has finished.
 */
@Composable
internal fun DangerZone(
    state: SettingsState,
    isPlay: Boolean,
    playDeletionContext: PlayDeletionContext,
    onOpenSubscription: () -> Unit,
    onSubmitVaultDelete: () -> Unit,
) {
    val tokens = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    val linkUnavailable = stringResource(R.string.peppy_toggle_android_link_unavailable)
    var vaultDeleteOpen by remember { mutableStateOf(false) }
    var erase by remember { mutableStateOf("") }
    var submitted by remember { mutableStateOf(false) }
    val vaultDeleteEligible = state.ownerRole
    val deleting = state.vaultDeleteInFlight
    LaunchedEffect(deleting, vaultDeleteEligible, submitted) {
        if (vaultDeleteOpen && !deleting && (!vaultDeleteEligible || submitted)) {
            erase = ""
            submitted = false
            vaultDeleteOpen = false
        }
    }

    SettingsGroup(tag = "settings-group-danger", title = stringResource(R.string.peppy_toggle_android_settings_danger), danger = true) {
        if (state.hosted) HostedAccountDeletionRow(
            tag = "account-delete-row",
            onOpenFailed = { state.failure = linkUnavailable },
            onViewSubscription = onOpenSubscription,
            isPlay = isPlay,
            playManaged = playDeletionContext.managed,
            playProductId = playDeletionContext.productId,
        )
        if (state.hosted && state.ownerRole) HorizontalDivider(color = tokens.error.copy(alpha = 0.4f))
        if (state.ownerRole) DestructiveRow(
            tag = "vault-delete-row",
            label = stringResource(R.string.peppy_vault_delete_row),
            consequence = stringResource(R.string.peppy_vault_delete_consequence),
            actionLabel = stringResource(R.string.peppy_settings_delete_action),
            onAction = { vaultDeleteOpen = true },
        )
    }

    if (vaultDeleteOpen) AlertDialog(
        onDismissRequest = { if (!deleting) { erase = ""; vaultDeleteOpen = false } },
        title = { Text(stringResource(R.string.peppy_vault_delete_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                Text(stringResource(R.string.peppy_vault_delete_consequence), style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(
                    value = erase,
                    onValueChange = { erase = it },
                    enabled = !deleting,
                    singleLine = true,
                    label = { Text(stringResource(R.string.peppy_vault_delete_confirm_field)) },
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, imeAction = ImeAction.Done),
                    modifier = Modifier.testTag("vault-delete-confirm-field"),
                )
                if (deleting) CircularProgressIndicator(Modifier.testTag("vault-delete-progress"))
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    submitted = true
                    onSubmitVaultDelete()
                },
                enabled = vaultDeleteEligible && erase == VAULT_ERASE_CONFIRMATION && !deleting,
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                    contentColor = MaterialTheme.colorScheme.onError,
                ),
                modifier = Modifier.testTag("vault-delete-final-button"),
            ) { Text(stringResource(R.string.peppy_vault_delete_row)) }
        },
        dismissButton = {
            TextButton(
                onClick = { erase = ""; vaultDeleteOpen = false },
                enabled = !deleting,
            ) { Text(stringResource(R.string.peppy_vault_delete_keep)) }
        },
        modifier = Modifier.testTag("vault-delete-dialog"),
    )
}
