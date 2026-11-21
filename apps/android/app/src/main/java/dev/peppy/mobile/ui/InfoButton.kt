package dev.peppy.mobile.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R

/**
 * Gives a 48dp (i) touch target a 24dp layout footprint, so it does not grow the text line it sits on.
 * The target overflows that footprint evenly above and below it.
 */
@Composable
internal fun InfoTargetFootprint(content: @Composable () -> Unit) {
    Box(Modifier.size(24.dp).wrapContentSize(unbounded = true), contentAlignment = Alignment.Center) { content() }
}

@Composable
fun InfoButton(topic: String, title: String, body: String, tag: String) {
    var visible by remember { mutableStateOf(false) }
    val accessibleName = stringResource(R.string.peppy_help_android_more_about, topic)

    IconButton(
        onClick = { visible = true },
        modifier = Modifier
            .testTag(tag)
            .semantics { contentDescription = accessibleName },
    ) {
        Icon(
            imageVector = Icons.Default.Info,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    if (visible) {
        AlertDialog(
            onDismissRequest = { visible = false },
            title = { Text(title) },
            text = {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    Text(body, style = MaterialTheme.typography.bodyMedium)
                }
            },
            confirmButton = {
                TextButton(onClick = { visible = false }, modifier = Modifier.testTag("$tag-dismiss")) {
                    Text(stringResource(R.string.peppy_help_dismiss))
                }
            },
            modifier = Modifier.testTag("$tag-dialog"),
        )
    }
}
