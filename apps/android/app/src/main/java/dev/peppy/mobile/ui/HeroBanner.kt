package dev.peppy.mobile.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.ui.theme.PeppyTokens

@Composable
internal fun HeroBanner(title: String, body: String, tag: String) {
    Column(Modifier.fillMaxWidth().background(PeppyTokens.Dark.surfaceChrome).testTag(tag).padding(24.dp)) {
        Canvas(Modifier.fillMaxWidth().height(72.dp).clearAndSetSemantics {}) {
            val accent = PeppyTokens.Dark.accent
            drawRoundRect(accent, topLeft = androidx.compose.ui.geometry.Offset(12.dp.toPx(), 8.dp.toPx()), size = androidx.compose.ui.geometry.Size(28.dp.toPx(), 56.dp.toPx()), style = Stroke(3.dp.toPx()))
            drawRoundRect(accent, topLeft = androidx.compose.ui.geometry.Offset(size.width - 76.dp.toPx(), 16.dp.toPx()), size = androidx.compose.ui.geometry.Size(64.dp.toPx(), 44.dp.toPx()), style = Stroke(3.dp.toPx()))
            val start = androidx.compose.ui.geometry.Offset(54.dp.toPx(), 36.dp.toPx())
            val end = androidx.compose.ui.geometry.Offset(size.width - 88.dp.toPx(), 36.dp.toPx())
            drawLine(accent, start, end, 3.dp.toPx(), pathEffect = androidx.compose.ui.graphics.PathEffect.dashPathEffect(floatArrayOf(8.dp.toPx(), 6.dp.toPx())))
            drawLine(accent, end, androidx.compose.ui.geometry.Offset(end.x - 10.dp.toPx(), end.y - 7.dp.toPx()), 3.dp.toPx())
            drawLine(accent, end, androidx.compose.ui.geometry.Offset(end.x - 10.dp.toPx(), end.y + 7.dp.toPx()), 3.dp.toPx())
        }
        Text(title, Modifier.padding(top = 16.dp).semantics { heading() }.testTag("$tag-title"), color = PeppyTokens.Dark.textStrong, style = MaterialTheme.typography.headlineSmall)
        Text(body, Modifier.padding(top = 8.dp).testTag("$tag-body"), color = PeppyTokens.Dark.textPrimary, style = MaterialTheme.typography.bodyLarge)
    }
}
