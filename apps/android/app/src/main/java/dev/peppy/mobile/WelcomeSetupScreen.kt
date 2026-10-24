package dev.peppy.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.ui.InfoButton

internal enum class WelcomeDestination {
    HOSTED,
    SELF_HOSTED,
    HOSTED_ENROLLMENT,
    SELF_HOSTED_ENROLLMENT,
    HOSTED_PAIRING,
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun WelcomeScreen(onGetStarted: () -> Unit, onSelectSelfHosted: () -> Unit) {
    var modeMenuOpen by remember { mutableStateOf(false) }
    val hosted = stringResource(R.string.peppy_onboarding_mode_hosted)

    Column(
        Modifier.fillMaxWidth().testTag("welcome-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Image(
            painter = painterResource(R.drawable.peppy_logo),
            contentDescription = null,
            modifier = Modifier.size(88.dp),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(stringResource(R.string.peppy_onboarding_headline), Modifier.weight(1f), style = MaterialTheme.typography.headlineSmall)
            InfoButton(
                stringResource(R.string.peppy_onboarding_headline),
                stringResource(R.string.peppy_onboarding_headline),
                listOf(
                    stringResource(R.string.peppy_onboarding_body),
                    stringResource(R.string.peppy_onboarding_hosted_sub),
                ).joinToString("\n\n"),
                "info-onboarding",
            )
        }
        ExposedDropdownMenuBox(
            expanded = modeMenuOpen,
            onExpandedChange = { modeMenuOpen = it },
            modifier = Modifier.fillMaxWidth().testTag("welcome-server-mode-selector"),
        ) {
            OutlinedTextField(
                value = hosted,
                onValueChange = {},
                readOnly = true,
                label = { Text(stringResource(R.string.peppy_onboarding_mode_selector)) },
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = modeMenuOpen) },
                modifier = Modifier.fillMaxWidth().menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable),
            )
            ExposedDropdownMenu(expanded = modeMenuOpen, onDismissRequest = { modeMenuOpen = false }) {
                DropdownMenuItem(
                    text = { Text(hosted) },
                    onClick = { modeMenuOpen = false },
                    modifier = Modifier.testTag("welcome-server-mode-hosted"),
                )
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.peppy_onboarding_mode_self_hosted)) },
                    onClick = {
                        modeMenuOpen = false
                        onSelectSelfHosted()
                    },
                    modifier = Modifier.testTag("welcome-server-mode-self-hosted"),
                )
            }
        }
        Button(onClick = onGetStarted, modifier = Modifier.fillMaxWidth().testTag("welcome-get-started-button")) {
            Text(stringResource(R.string.peppy_onboarding_hosted_cta))
        }
    }
}

@Composable
internal fun SelfHostedSetupScreen(
    onBack: () -> Unit,
    onEnroll: () -> Unit,
    onOpenGuide: () -> Unit,
) {
    BackHandler(onBack = onBack)

    Column(
        Modifier.fillMaxWidth().testTag("self-hosted-screen"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        OutlinedButton(onClick = onBack, modifier = Modifier.testTag("self-hosted-back-button")) {
            Text(stringResource(R.string.peppy_back))
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(stringResource(R.string.peppy_self_hosted_headline), Modifier.weight(1f), style = MaterialTheme.typography.headlineSmall)
            InfoButton(
                stringResource(R.string.peppy_self_hosted_headline),
                stringResource(R.string.peppy_self_hosted_headline),
                stringResource(R.string.peppy_self_hosted_body),
                "info-self-hosted",
            )
        }
        Button(onClick = onEnroll, modifier = Modifier.fillMaxWidth().testTag("self-hosted-enroll-button")) { Text(stringResource(R.string.peppy_continue)) }
        OutlinedButton(onClick = onOpenGuide, modifier = Modifier.testTag("self-hosted-docs-link")) {
            Text(stringResource(R.string.peppy_self_hosted_docs_link))
        }
    }
}
