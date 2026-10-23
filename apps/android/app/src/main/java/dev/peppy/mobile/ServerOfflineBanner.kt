package dev.peppy.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.ui.theme.PeppyTokens

@Composable
internal fun ServerOfflineBanner() {
    val status by ServerReachabilityState.state.collectAsState()
    if (status != ServerReachability.UNAVAILABLE) return
    val colors = if (androidx.compose.foundation.isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("server-offline-banner").semantics { liveRegion = LiveRegionMode.Polite },
        color = colors.warningSurface,
        contentColor = colors.warningText,
        border = BorderStroke(1.dp, colors.warningBorder),
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
            Text(stringResource(R.string.peppy_server_offline_title), style = MaterialTheme.typography.titleSmall)
            Text(stringResource(R.string.peppy_server_offline_body), style = MaterialTheme.typography.bodySmall)
        }
    }
}
