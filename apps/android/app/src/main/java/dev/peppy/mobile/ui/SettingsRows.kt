package dev.peppy.mobile.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.layout.SubcomposeLayout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.ui.theme.PeppyColorScheme
import dev.peppy.mobile.ui.theme.PeppyTokens

internal data class InfoSpec(val title: String, val body: String, val tag: String)

internal enum class BadgeTone { NEUTRAL, SUCCESS, WARNING }

internal data class BadgeSpec(val text: String, val tag: String, val tone: BadgeTone = BadgeTone.NEUTRAL)

private val RowCorner = RoundedCornerShape(8.dp)
private val BadgeCorner = RoundedCornerShape(4.dp)
private val BadgeSpacing = 4.dp

@Composable
private fun peppyTokens(onDarkBand: Boolean = false): PeppyColorScheme =
    if (onDarkBand || isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light

private fun isStackedLayout(maxWidth: Dp, fontScale: Float): Boolean =
    maxWidth < 312.dp || fontScale > 1.15f

@Composable
internal fun SettingsGroup(
    tag: String,
    title: String?,
    showDivider: Boolean = true,
    danger: Boolean = false,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag(tag)) {
        if (showDivider) {
            HorizontalDivider()
            Spacer(Modifier.height(8.dp))
        }
        if (title != null) {
            Text(
                title,
                Modifier.padding(bottom = 4.dp).semantics { heading() },
                style = MaterialTheme.typography.titleMedium,
            )
        }
        if (danger) {
            val tokens = peppyTokens()
            Column(
                Modifier
                    .fillMaxWidth()
                    .border(1.dp, tokens.error, RowCorner)
                    .background(tokens.errorSurface, RowCorner)
                    .padding(horizontal = 12.dp, vertical = 4.dp),
                content = content,
            )
        } else {
            content()
        }
    }
}

@Composable
internal fun NavigationRow(
    tag: String,
    label: String,
    onClick: () -> Unit,
    icon: ImageVector? = null,
    summary: String? = null,
    attention: String? = null,
    info: InfoSpec? = null,
) {
    NavRow(
        tag = tag,
        label = label,
        onClick = onClick,
        onClickLabel = null,
        icon = icon,
        summary = summary,
        info = info,
        attention = attention,
        trailingIcon = Icons.AutoMirrored.Filled.KeyboardArrowRight,
        trailingSize = 24.dp,
    )
}

@Composable
internal fun ExternalLinkRow(
    tag: String,
    label: String,
    onClick: () -> Unit,
    icon: ImageVector? = null,
    summary: String? = null,
    info: InfoSpec? = null,
    attention: String? = null,
) {
    NavRow(
        tag = tag,
        label = label,
        onClick = onClick,
        onClickLabel = stringResource(R.string.peppy_about_opens_in_browser),
        icon = icon,
        summary = summary,
        info = info,
        attention = attention,
        trailingIcon = Icons.AutoMirrored.Filled.OpenInNew,
        trailingSize = 20.dp,
    )
}

@Composable
private fun NavRow(
    tag: String,
    label: String,
    onClick: () -> Unit,
    onClickLabel: String?,
    icon: ImageVector?,
    summary: String?,
    info: InfoSpec?,
    attention: String?,
    trailingIcon: ImageVector,
    trailingSize: Dp,
) {
    val minHeight = if (summary != null) 56.dp else 48.dp
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val stacked = isStackedLayout(maxWidth, LocalDensity.current.fontScale)
        val leading: @Composable () -> Unit = {
            icon?.let { Icon(it, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(24.dp)) }
        }
        val trailing: @Composable () -> Unit = {
            if (!stacked) attention?.let { PeppyBadge(BadgeSpec(it, "$tag-attention", BadgeTone.WARNING)) }
            TrailingIcon(trailingIcon, trailingSize, "$tag-trailing")
        }
        val labelAttention = if (stacked) attention else null
        if (info == null) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = minHeight)
                    .clip(RowCorner)
                    .clickable(role = Role.Button, onClickLabel = onClickLabel, onClick = onClick)
                    .testTag(tag),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                leading()
                NavLabel(tag, label, summary, labelAttention, Modifier.weight(1f))
                trailing()
            }
        } else {
            Box(Modifier.fillMaxWidth().heightIn(min = minHeight)) {
                // Touch-only overlay: no semantics and no focus, so TalkBack sees the label region and the (i) only.
                Box(Modifier.matchParentSize().clearAndSetSemantics {}) {
                    Box(
                        Modifier
                            .matchParentSize()
                            .clip(RowCorner)
                            .focusProperties { canFocus = false }
                            .clickable(
                                interactionSource = remember { MutableInteractionSource() },
                                indication = LocalIndication.current,
                                onClick = onClick,
                            ),
                    )
                }
                Row(
                    Modifier.fillMaxWidth().heightIn(min = minHeight),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                        Row(
                            Modifier
                                .weight(1f, fill = false)
                                .heightIn(min = minHeight)
                                .clip(RowCorner)
                                .clickable(role = Role.Button, onClickLabel = onClickLabel, onClick = onClick)
                                .semantics { if (!stacked) attention?.let { stateDescription = it } }
                                .testTag(tag),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(16.dp),
                        ) {
                            leading()
                            NavLabel(tag, label, summary, labelAttention, Modifier)
                        }
                        InfoButton(info.title, info.title, info.body, info.tag)
                    }
                    if (!stacked) attention?.let {
                        PeppyBadge(BadgeSpec(it, "$tag-attention", BadgeTone.WARNING), modifier = Modifier.semantics { hideFromAccessibility() })
                    }
                    TrailingIcon(trailingIcon, trailingSize, "$tag-trailing")
                }
            }
        }
    }
}

/** Trailing nav glyph in a fixed 24dp slot, so chevrons (24dp) and external-link icons (20dp) align. */
@Composable
private fun TrailingIcon(icon: ImageVector, iconSize: Dp, tag: String) {
    Box(Modifier.size(24.dp), contentAlignment = Alignment.Center) {
        Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(iconSize).testTag(tag))
    }
}

@Composable
private fun NavLabel(tag: String, label: String, summary: String?, attention: String?, modifier: Modifier) {
    Column(modifier.padding(vertical = if (summary != null || attention != null) 8.dp else 0.dp)) {
        Text(label, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurface)
        summary?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        attention?.let { PeppyBadge(BadgeSpec(it, "$tag-attention", BadgeTone.WARNING)) }
    }
}

@Composable
internal fun ActionRow(
    tag: String,
    label: String,
    actionLabel: String,
    onAction: () -> Unit,
    icon: ImageVector? = null,
    info: InfoSpec? = null,
    warning: Boolean = false,
    enabled: Boolean = true,
) {
    val tokens = peppyTokens()
    val leading: @Composable () -> Unit = {
        icon?.let {
            Icon(
                it,
                contentDescription = null,
                tint = if (warning) tokens.warningText else MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(24.dp),
            )
        }
    }
    val labelRow: @Composable (Modifier) -> Unit = { modifier ->
        Row(modifier, verticalAlignment = Alignment.CenterVertically) {
            Text(label, Modifier.weight(1f, fill = false), style = MaterialTheme.typography.bodyLarge)
            info?.let { InfoTargetFootprint { InfoButton(it.title, it.title, it.body, it.tag) } }
        }
    }
    val button: @Composable () -> Unit = {
        OutlinedButton(
            onClick = onAction,
            enabled = enabled,
            contentPadding = PaddingValues(horizontal = 16.dp),
            modifier = Modifier.testTag("$tag-button"),
        ) {
            Text(actionLabel)
        }
    }
    BoxWithConstraints(Modifier.fillMaxWidth().heightIn(min = 48.dp).padding(vertical = 8.dp).testTag(tag)) {
        if (isStackedLayout(maxWidth, LocalDensity.current.fontScale)) {
            Column(Modifier.fillMaxWidth()) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                    leading()
                    labelRow(Modifier.weight(1f))
                }
                Box(Modifier.padding(start = if (icon != null) 40.dp else 0.dp, top = 8.dp)) { button() }
            }
        } else {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                leading()
                labelRow(Modifier.weight(1f))
                button()
            }
        }
    }
}

@Composable
internal fun DestructiveRow(
    tag: String,
    label: String,
    consequence: String,
    actionLabel: String,
    onAction: () -> Unit,
    enabled: Boolean = true,
) {
    val colors = MaterialTheme.colorScheme
    val text: @Composable (Modifier) -> Unit = { modifier ->
        Column(modifier.semantics(mergeDescendants = true) {}) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            Text(consequence, style = MaterialTheme.typography.bodyMedium, color = colors.onSurfaceVariant)
        }
    }
    val button: @Composable (Modifier) -> Unit = { modifier ->
        Button(
            onClick = onAction,
            enabled = enabled,
            colors = ButtonDefaults.buttonColors(containerColor = colors.error, contentColor = colors.onError),
            contentPadding = PaddingValues(horizontal = 16.dp),
            modifier = modifier.semantics { contentDescription = label }.testTag("$tag-button"),
        ) {
            Text(actionLabel)
        }
    }
    Column(Modifier.fillMaxWidth().heightIn(min = 56.dp).testTag(tag).padding(vertical = 8.dp)) {
        text(Modifier.fillMaxWidth())
        button(Modifier.padding(top = 8.dp))
    }
}

@Composable
internal fun AttentionCallout(
    tag: String,
    text: String,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
) {
    val tokens = peppyTokens()
    val message: @Composable (Modifier) -> Unit = { modifier ->
        Row(
            modifier.semantics(mergeDescendants = true) {},
            verticalAlignment = Alignment.Top,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(Icons.Default.Warning, contentDescription = null, tint = tokens.warningText, modifier = Modifier.padding(top = 2.dp).size(20.dp))
            Text(text, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurface)
        }
    }
    val action: @Composable () -> Unit = {
        if (actionLabel != null && onAction != null) {
            TextButton(
                onClick = onAction,
                colors = ButtonDefaults.textButtonColors(contentColor = tokens.warningText),
                modifier = Modifier.testTag("$tag-action"),
            ) {
                Text(actionLabel)
            }
        }
    }
    BoxWithConstraints(
        Modifier
            .fillMaxWidth()
            .testTag(tag)
            .border(1.dp, tokens.warningBorder, RowCorner)
            .background(tokens.warningSurface, RowCorner)
            .padding(12.dp),
    ) {
        if (isStackedLayout(maxWidth, LocalDensity.current.fontScale)) {
            Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                message(Modifier.fillMaxWidth())
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) { action() }
            }
        } else {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                message(Modifier.weight(1f))
                action()
            }
        }
    }
}

@Composable
internal fun PeppyBadge(spec: BadgeSpec, onDarkBand: Boolean = false, modifier: Modifier = Modifier) {
    val tokens = peppyTokens(onDarkBand)
    val warning = spec.tone == BadgeTone.WARNING
    Row(
        modifier
            .background(if (warning) tokens.warningSurface else tokens.surfaceActive, BadgeCorner)
            .padding(horizontal = 6.dp, vertical = 2.dp)
            .testTag(spec.tag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(BadgeSpacing),
    ) {
        when (spec.tone) {
            BadgeTone.NEUTRAL -> Unit
            BadgeTone.SUCCESS -> Box(Modifier.size(6.dp).background(tokens.success, CircleShape))
            BadgeTone.WARNING -> Icon(
                Icons.Default.Warning,
                contentDescription = null,
                tint = tokens.warningText,
                modifier = Modifier.size(12.dp),
            )
        }
        Text(
            spec.text,
            color = if (warning) tokens.warningText else tokens.textPrimary,
            style = MaterialTheme.typography.labelMedium,
            maxLines = 1,
            softWrap = false,
            overflow = TextOverflow.Clip,
        )
    }
}

@Composable
internal fun SingleLineBadges(
    badges: List<BadgeSpec>,
    overflowText: (Int) -> String,
    tag: String,
    modifier: Modifier = Modifier,
) {
    val description = badges.joinToString(", ") { it.text }
    SubcomposeLayout(
        modifier.testTag(tag).semantics(mergeDescendants = true) { contentDescription = description },
    ) { constraints ->
        val spacing = BadgeSpacing.roundToPx()
        val hiddenBadge = Modifier.semantics { hideFromAccessibility() }
        val badgeLayouts = badges.mapIndexed { index, badge ->
            subcompose("badge-$index") { PeppyBadge(badge, modifier = hiddenBadge) }.first().measure(Constraints())
        }
        fun overflowLayout(hiddenCount: Int): Placeable = subcompose("overflow-$hiddenCount") {
            PeppyBadge(BadgeSpec(overflowText(hiddenCount), "$tag-more"), modifier = hiddenBadge)
        }.first().measure(Constraints())
        fun rowWidth(items: List<Placeable>): Int =
            items.sumOf { it.width } + spacing * (items.size - 1).coerceAtLeast(0)

        var visible: List<Placeable> = badgeLayouts
        if (rowWidth(visible) > constraints.maxWidth) {
            visible = listOf(overflowLayout(badges.size))
            for (shown in badges.size - 1 downTo 1) {
                val candidate = badgeLayouts.take(shown) + overflowLayout(badges.size - shown)
                if (rowWidth(candidate) <= constraints.maxWidth) {
                    visible = candidate
                    break
                }
            }
        }
        val width = minOf(rowWidth(visible), constraints.maxWidth)
        val height = visible.maxOfOrNull { it.height } ?: 0
        layout(width, height) {
            var x = 0
            visible.forEach { placeable ->
                placeable.placeRelative(x, (height - placeable.height) / 2)
                x += placeable.width + spacing
            }
        }
    }
}
