package dev.peppy.mobile.about

import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.R
import dev.peppy.mobile.Section
import dev.peppy.mobile.StatusRow
import dev.peppy.mobile.gateway.AccountNavigationRow
import dev.peppy.mobile.ui.ScreenBody

internal data class AboutBuildInfo(val versionName: String, val versionCode: Int, val sourceCommit: String, val dirtyState: String) {
    companion object {
        fun fromBuildConfig() = AboutBuildInfo(BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE, BuildConfig.PEPPY_SOURCE_COMMIT, BuildConfig.PEPPY_SOURCE_DIRTY_STATE)
    }
}

@Composable
internal fun AboutScreen(
    buildInfo: AboutBuildInfo,
    onBack: () -> Unit,
    onOpenLink: (String) -> Unit,
    onOpenLicenses: () -> Unit,
    licensesAvailable: Boolean,
    linkError: String? = null,
) {
    ScreenBody("about-body") {
        TextButton(onClick = onBack, modifier = Modifier.testTag("about-back-button")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.peppy_back)) }
        Text(stringResource(R.string.peppy_about_title), Modifier.testTag("about-title").semantics { heading() }, style = MaterialTheme.typography.headlineSmall)
        Section("about-build-section", stringResource(R.string.peppy_about_section_build), showDivider = false) {
            StatusRow("about-version-name", stringResource(R.string.peppy_about_version_label), buildInfo.versionName)
            StatusRow("about-version-code", stringResource(R.string.peppy_about_build_number_label), buildInfo.versionCode.toString())
            Column(Modifier.fillMaxWidth().testTag("about-source-commit")) {
                Text(stringResource(R.string.peppy_about_source_commit_label), style = MaterialTheme.typography.bodyMedium)
                SelectionContainer { Text(buildInfo.sourceCommit, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodyMedium) }
            }
            if (buildInfo.dirtyState == "true") Text(stringResource(R.string.peppy_about_source_modified), Modifier.testTag("about-source-modified"))
        }
        Section("about-links-section", stringResource(R.string.peppy_about_section_links)) {
            AccountNavigationRow("about-privacy-row", stringResource(R.string.peppy_privacy_policy), true) { onOpenLink(PRIVACY_URL) }
            AccountNavigationRow("about-source-row", stringResource(R.string.peppy_about_source_code), true) { onOpenLink(SOURCE_URL) }
            if (licensesAvailable) AccountNavigationRow("about-licenses-row", stringResource(R.string.peppy_about_open_source_licenses), onOpenLicenses)
        }
        linkError?.let { Text(it, Modifier.testTag("account-error")) }
    }
}

internal const val PRIVACY_URL = "https://peppy.pro/privacy"
internal const val SOURCE_URL = "https://github.com/mattv8/peppy"
