package dev.peppy.mobile.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.theme.PeppyTokens

@Composable
internal fun DetailsPageHeader(title: String, tag: String, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    Column(Modifier.fillMaxWidth().testTag("$tag-header")) {
        TextButton(
            onClick = onBack,
            modifier = Modifier.heightIn(min = 48.dp).testTag("$tag-back"),
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.peppy_back))
        }
        Text(
            title,
            Modifier.semantics { heading() }.testTag("$tag-title"),
            style = MaterialTheme.typography.titleLarge,
        )
    }
}

@Composable
internal fun DetailsNavigationRow(tag: String, label: String, attention: String?, onClick: () -> Unit) {
    TextButton(
        onClick = onClick,
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag),
        colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onSurface),
    ) {
        Text(label, Modifier.weight(1f))
        attention?.let { NeedsAttentionBadge("$tag-attention", it) }
        Text("›", Modifier.clearAndSetSemantics {})
    }
}

@Composable
internal fun DetailsStatusRow(tag: String, label: String, value: String) {
    val fontScale = LocalDensity.current.fontScale
    BoxWithConstraints(Modifier.fillMaxWidth().testTag(tag)) {
        if (maxWidth < 360.dp || fontScale > 1.15f) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(value, style = MaterialTheme.typography.bodyMedium)
            }
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                Text(value, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.End)
            }
        }
    }
}

@Composable
private fun NeedsAttentionBadge(tag: String, text: String) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    Row(
        Modifier
            .padding(end = 8.dp)
            .background(colors.warningSurface, RoundedCornerShape(8.dp))
            .padding(horizontal = 6.dp, vertical = 2.dp)
            .testTag(tag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text("⚠", Modifier.clearAndSetSemantics {}, color = colors.warningText, style = MaterialTheme.typography.labelSmall)
        Text(text, color = colors.warningText, style = MaterialTheme.typography.labelSmall)
    }
}
