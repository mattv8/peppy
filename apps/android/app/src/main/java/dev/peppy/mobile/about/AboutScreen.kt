package dev.peppy.mobile.about

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.DetailPage
import dev.peppy.mobile.ui.DetailsStatusRow
import dev.peppy.mobile.ui.ExternalLinkRow
import dev.peppy.mobile.ui.InfoSpec
import dev.peppy.mobile.ui.NavigationRow
import dev.peppy.mobile.ui.PageChrome
import dev.peppy.mobile.ui.SettingsGroup

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
    DetailPage(PageChrome(stringResource(R.string.peppy_about_title), "about", onBack)) {
        SettingsGroup("about-build-section", stringResource(R.string.peppy_about_section_build), showDivider = false) {
            DetailsStatusRow("about-version-name", stringResource(R.string.peppy_about_version_label), buildInfo.versionName)
            DetailsStatusRow("about-version-code", stringResource(R.string.peppy_about_build_number_label), buildInfo.versionCode.toString())
            Column(Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(vertical = 8.dp).testTag("about-source-commit")) {
                Text(
                    stringResource(R.string.peppy_about_source_commit_label),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                SelectionContainer {
                    Text(
                        buildInfo.sourceCommit,
                        fontFamily = FontFamily.Monospace,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
            if (buildInfo.dirtyState == "true") {
                Text(
                    stringResource(R.string.peppy_about_source_modified),
                    Modifier.padding(vertical = 8.dp).testTag("about-source-modified"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        SettingsGroup("about-links-section", stringResource(R.string.peppy_about_section_links)) {
            ExternalLinkRow(
                tag = "about-privacy-row",
                label = stringResource(R.string.peppy_privacy_policy),
                onClick = { onOpenLink(PRIVACY_URL) },
                info = InfoSpec(
                    title = stringResource(R.string.peppy_privacy_policy),
                    body = stringResource(R.string.peppy_privacy_policy_scope),
                    tag = "info-about-privacy",
                ),
            )
            ExternalLinkRow("about-source-row", stringResource(R.string.peppy_about_source_code), onClick = { onOpenLink(SOURCE_URL) })
            if (licensesAvailable) NavigationRow("about-licenses-row", stringResource(R.string.peppy_about_open_source_licenses), onClick = onOpenLicenses)
        }
        linkError?.let {
            Text(
                it,
                Modifier.testTag("account-error").semantics { liveRegion = LiveRegionMode.Polite },
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

internal const val PRIVACY_URL = "https://peppy.pro/privacy"
internal const val GUIDES_URL = "https://peppy.pro/docs"
internal const val SOURCE_URL = "https://github.com/mattv8/peppy"
