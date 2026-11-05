package dev.peppy.mobile.about

import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import dev.peppy.mobile.R
import dev.peppy.mobile.Section
import dev.peppy.mobile.gateway.AccountNavigationRow
import dev.peppy.mobile.ui.ScreenBody
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private sealed interface TextLoad { data object Loading : TextLoad; data class Value(val text: String) : TextLoad; data object Error : TextLoad }

@Composable
internal fun LicensesScreen(notices: ThirdPartyNotices, onBack: () -> Unit, onOpenLicense: (ThirdPartyNoticeEntry) -> Unit) {
    var retry by remember { mutableIntStateOf(0) }
    val notice by produceState<TextLoad>(TextLoad.Loading, notices, retry) {
        value = runCatching { withContext(Dispatchers.IO) { notices.notice() } }.fold({ TextLoad.Value(it) }, { TextLoad.Error })
    }
    ScreenBody("licenses-body") {
        TextButton(onClick = onBack, modifier = Modifier.testTag("licenses-back-button")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.peppy_back)) }
        Text(stringResource(R.string.peppy_about_licenses_title), Modifier.testTag("licenses-title").semantics { heading() }, style = MaterialTheme.typography.headlineSmall)
        when (val state = notice) {
            TextLoad.Loading -> Loading("licenses-loading")
            TextLoad.Error -> LoadError("licenses-error") { retry++ }
            is TextLoad.Value -> {
                Section("licenses-notices-section", stringResource(R.string.peppy_about_notices_heading)) { SelectionContainer { Text(state.text, Modifier.testTag("licenses-notice"), fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) } }
                val entries = remember(notices) { notices.entries() }
                Section("licenses-texts-section", stringResource(R.string.peppy_about_license_texts_heading)) { entries.forEach { entry -> AccountNavigationRow("license-row-${entry.fileName.substringBeforeLast('.')}", entry.title()) { onOpenLicense(entry) } } }
            }
        }
    }
}

@Composable
internal fun LicenseTextScreen(notices: ThirdPartyNotices, entry: ThirdPartyNoticeEntry, onBack: () -> Unit) {
    var retry by remember { mutableIntStateOf(0) }
    val text by produceState<TextLoad>(TextLoad.Loading, notices, entry, retry) {
        value = runCatching { withContext(Dispatchers.IO) { notices.read(entry.fileName) } }.fold({ TextLoad.Value(it) }, { TextLoad.Error })
    }
    ScreenBody("license-body") {
        TextButton(onClick = onBack, modifier = Modifier.testTag("license-back-button")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.peppy_back)) }
        Text(entry.title(), Modifier.testTag("license-title").semantics { heading() }, style = MaterialTheme.typography.headlineSmall)
        when (val state = text) {
            TextLoad.Loading -> Loading("license-loading")
            TextLoad.Error -> LoadError("license-error") { retry++ }
            is TextLoad.Value -> SelectionContainer { Text(state.text, Modifier.testTag("license-text"), fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) }
        }
    }
}

@Composable private fun ThirdPartyNoticeEntry.title() = titleRes?.let { stringResource(it) } ?: fileName.substringBeforeLast('.')
@Composable private fun Loading(tag: String) {
    val label = stringResource(R.string.peppy_about_loading)
    CircularProgressIndicator(Modifier.testTag(tag).semantics { contentDescription = label })
}
@Composable private fun LoadError(tag: String, onRetry: () -> Unit) { Text(stringResource(R.string.peppy_about_license_load_failed), Modifier.testTag(tag)); TextButton(onClick = onRetry) { Text(stringResource(R.string.peppy_toggle_android_retry)) } }
