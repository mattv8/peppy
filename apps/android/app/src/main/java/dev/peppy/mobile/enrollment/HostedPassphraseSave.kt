package dev.peppy.mobile.enrollment

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.credentials.CreatePasswordRequest
import androidx.credentials.CredentialManager
import androidx.credentials.exceptions.CreateCredentialCancellationException
import androidx.credentials.exceptions.CreateCredentialException
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.InfoButton
import kotlinx.coroutines.CancellationException

internal fun interface PassphrasePasswordSaver {
    suspend fun save(username: String, phrase: String): PassphraseSaveOutcome
}

internal enum class PassphraseSaveOutcome {
    SAVED,
    CANCELLED,
    FAILED,
}

internal class CredentialManagerPassphraseSaver(
    private val activity: ComponentActivity,
) : PassphrasePasswordSaver {
    override suspend fun save(username: String, phrase: String): PassphraseSaveOutcome = try {
        CredentialManager.create(activity).createCredential(
            activity,
            CreatePasswordRequest(username, phrase),
        )
        PassphraseSaveOutcome.SAVED
    } catch (_: CreateCredentialCancellationException) {
        PassphraseSaveOutcome.CANCELLED
    } catch (_: CreateCredentialException) {
        PassphraseSaveOutcome.FAILED
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        PassphraseSaveOutcome.FAILED
    }
}

internal enum class PassphraseSaveState {
    IDLE,
    SAVING,
    SAVED,
    FAILED,
}

internal class PassphraseSaveTracker {
    var status by mutableStateOf(PassphraseSaveState.IDLE)
        private set
    private var generation by mutableLongStateOf(0L)

    fun begin(): Long? {
        if (status == PassphraseSaveState.SAVING) return null
        status = PassphraseSaveState.SAVING
        return generation
    }

    fun finish(saveGeneration: Long, outcome: PassphraseSaveOutcome) {
        if (saveGeneration != generation || status != PassphraseSaveState.SAVING) return
        status = when (outcome) {
            PassphraseSaveOutcome.SAVED -> PassphraseSaveState.SAVED
            PassphraseSaveOutcome.CANCELLED -> PassphraseSaveState.IDLE
            PassphraseSaveOutcome.FAILED -> PassphraseSaveState.FAILED
        }
    }

    fun abandon(saveGeneration: Long) {
        if (saveGeneration == generation && status == PassphraseSaveState.SAVING) {
            status = PassphraseSaveState.IDLE
        }
    }

    fun reset() {
        generation++
        status = PassphraseSaveState.IDLE
    }
}

@Composable
internal fun HostedPassphraseStep(
    phrase: String,
    acknowledged: Boolean,
    saveState: PassphraseSaveState,
    onAcknowledgedChange: (Boolean) -> Unit,
    onSave: () -> Unit,
    onContinue: () -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            stringResource(R.string.peppy_passphrase_create_headline),
            Modifier.weight(1f).semantics { heading() },
            style = MaterialTheme.typography.titleLarge,
        )
        InfoButton(
            topic = stringResource(R.string.peppy_passphrase_create_headline),
            title = stringResource(R.string.peppy_passphrase_create_headline),
            body = stringResource(R.string.peppy_passphrase_create_body),
            tag = "info-passphrase-create",
        )
    }
    Text(
        stringResource(R.string.peppy_passphrase_irrecoverable_warn),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.bodyMedium,
    )
    PassphraseDisplay(phrase)
    Column(
        verticalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier
            .semantics { liveRegion = LiveRegionMode.Polite }
            .testTag("passphrase-save-password-manager-section"),
    ) {
        if (saveState == PassphraseSaveState.SAVED) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.testTag("passphrase-save-password-manager-saved"),
            ) {
                Icon(
                    Icons.Default.Check,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    stringResource(R.string.peppy_passphrase_saved_password_manager),
                    Modifier.padding(start = 8.dp),
                )
            }
        } else {
            FilledTonalButton(
                onClick = onSave,
                enabled = saveState != PassphraseSaveState.SAVING,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .testTag("passphrase-save-password-manager"),
            ) {
                if (saveState == PassphraseSaveState.SAVING) {
                    CircularProgressIndicator(
                        Modifier.padding(end = 8.dp).size(18.dp),
                        strokeWidth = 2.dp,
                    )
                }
                Text(stringResource(R.string.peppy_passphrase_save_password_manager))
            }
            Text(
                stringResource(R.string.peppy_passphrase_save_password_manager_note),
                Modifier.testTag("passphrase-save-password-manager-note"),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodySmall,
            )
            if (saveState == PassphraseSaveState.FAILED) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.testTag("passphrase-save-password-manager-failed"),
                ) {
                    Icon(
                        Icons.Default.ErrorOutline,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                        tint = MaterialTheme.colorScheme.error,
                    )
                    Text(
                        stringResource(R.string.peppy_passphrase_save_password_manager_failed),
                        Modifier.padding(start = 8.dp),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .toggleable(
                value = acknowledged,
                role = Role.Checkbox,
                onValueChange = onAcknowledgedChange,
            )
            .testTag("passphrase-saved-checkbox"),
    ) {
        Checkbox(checked = acknowledged, onCheckedChange = null)
        Text(
            stringResource(R.string.peppy_passphrase_ack_label),
            Modifier.padding(start = 8.dp),
        )
    }
    Button(
        onClick = onContinue,
        enabled = acknowledged,
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .testTag("passphrase-continue-button"),
    ) {
        Text(stringResource(R.string.peppy_continue))
    }
}

@Composable
private fun PassphraseDisplay(phrase: String) {
    val words = phrase.trim().split(Regex("\\s+")).filter(String::isNotBlank)
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = MaterialTheme.shapes.medium,
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) { contentDescription = phrase }
            .testTag("passphrase-display"),
    ) {
        Row(
            Modifier.padding(16.dp),
            horizontalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            PassphraseWordColumn(words.take((words.size + 1) / 2), 0, Modifier.weight(1f))
            PassphraseWordColumn(words.drop((words.size + 1) / 2), (words.size + 1) / 2, Modifier.weight(1f))
        }
    }
}

@Composable
private fun PassphraseWordColumn(words: List<String>, startIndex: Int, modifier: Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        words.forEachIndexed { index, word ->
            Row {
                Text(
                    "${startIndex + index + 1}.",
                    Modifier.width(28.dp),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    word,
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}
