package dev.peppy.mobile.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConsentSheet(
    title: String,
    body: String,
    tag: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
    confirmLabel: String,
    nextStep: String? = null,
    disclosureTag: String? = null,
    showPrivacyPolicy: Boolean = true,
    dismissLabel: String? = null,
) {
    var confirmed by remember { mutableStateOf(false) }
    val uriHandler = LocalUriHandler.current
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        modifier = Modifier.testTag(tag),
    ) {
        Column(Modifier.padding(horizontal = 24.dp).padding(bottom = 16.dp)) {
            Text(title, Modifier.semantics { heading() }.testTag("$tag-title"), style = MaterialTheme.typography.titleLarge)
            Column(
                Modifier.fillMaxWidth().padding(top = 16.dp).weight(1f, fill = false)
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                Text(body, Modifier.testTag(disclosureTag ?: "$tag-body"), style = MaterialTheme.typography.bodyLarge)
                nextStep?.let { Text(it, Modifier.testTag("$tag-next-step"), style = MaterialTheme.typography.bodyMedium) }
                if (showPrivacyPolicy) {
                    TextButton(onClick = { uriHandler.openUri("https://peppy.pro/privacy") }, modifier = Modifier.testTag("$tag-policy")) {
                        Text(stringResource(R.string.peppy_privacy_policy))
                    }
                    Text(stringResource(R.string.peppy_privacy_policy_scope), style = MaterialTheme.typography.bodyMedium)
                }
            }
            HorizontalDivider(Modifier.padding(top = 12.dp))
            Button(
                onClick = { if (!confirmed) { confirmed = true; onConfirm() } },
                enabled = !confirmed,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp).testTag("$tag-confirm"),
            ) { Text(confirmLabel) }
            OutlinedButton(onClick = onDismiss, modifier = Modifier.fillMaxWidth().padding(top = 8.dp).testTag("$tag-dismiss")) {
                Text(dismissLabel ?: stringResource(R.string.peppy_toggle_android_not_now))
            }
        }
    }
}
