package dev.peppy.mobile.mms

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.R

internal fun restrictedMessageAppOpCommand(packageName: String): String =
    "adb shell appops set $packageName $READ_RESTRICTED_MESSAGES_OP allow"

@Composable
internal fun RestrictedAccessCommand(context: Context) {
    val command = remember(context.packageName) { restrictedMessageAppOpCommand(context.packageName) }
    var commandCopied by remember { mutableStateOf(false) }

    Text(command, Modifier.testTag("mms-restricted-message-command"), style = MaterialTheme.typography.bodySmall)
    OutlinedButton(
        onClick = {
            val clipboard = context.getSystemService(ClipboardManager::class.java)
            commandCopied = try {
                clipboard?.setPrimaryClip(ClipData.newPlainText("Restricted message access command", command))
                clipboard != null
            } catch (_: Exception) {
                false
            }
        },
        modifier = Modifier.testTag("mms-restricted-message-copy-command"),
    ) { Text(stringResource(R.string.peppy_mms_restricted_copy_command)) }
    if (commandCopied) {
        Text(
            stringResource(R.string.peppy_mms_restricted_command_copied),
            Modifier.testTag("mms-restricted-message-command-copied"),
            style = MaterialTheme.typography.bodySmall,
        )
    }
}
