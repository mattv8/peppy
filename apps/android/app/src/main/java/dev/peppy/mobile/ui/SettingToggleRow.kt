package dev.peppy.mobile.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp

@Composable
internal fun SettingToggleRow(
    title: String,
    checked: Boolean,
    enabled: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    tag: String,
    subtitle: String? = null,
    onClickLabel: String? = null,
    stateDescription: String? = null,
) {
    val settingsStateDescription = stateDescription
    Row(
        Modifier.fillMaxWidth().defaultMinSize(minHeight = 64.dp).toggleable(
            value = checked,
            enabled = enabled,
            role = Role.Switch,
            onValueChange = onCheckedChange,
        ).semantics {
            onClickLabel?.let { label ->
                onClick(label = label) {
                    if (enabled) {
                        onCheckedChange(!checked)
                        true
                    } else false
                }
            }
            settingsStateDescription?.let { this.stateDescription = it }
        }.testTag(tag).padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f).padding(end = 16.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            subtitle?.let { Text(it, Modifier.testTag("$tag-subtitle"), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium) }
        }
        Switch(checked, onCheckedChange = null, enabled = enabled, modifier = Modifier.testTag("$tag-switch"))
    }
}
