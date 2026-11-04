package dev.peppy.mobile.restrictedsetup

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.testTag
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.peppy.mobile.advanced.readAdvancedAccess
import dev.peppy.mobile.restrictedsetup.ui.RestrictedSetupPanel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
internal fun RestrictedSetupEntry(onAccessChanged: () -> Unit) {
    val context = LocalContext.current
    if (!RestrictedSetupRuntime.isSupported(context)) return
    var capabilityReadback by remember { mutableStateOf<RestrictedSetupReadback?>(null) }
    LaunchedEffect(Unit) {
        capabilityReadback = withContext(Dispatchers.IO) { readAdvancedAccess(context).toSetupReadback() }
    }
    val currentReadback = capabilityReadback ?: return
    val controller = RestrictedSetupRuntime.controller(context)
    val state by controller.state.collectAsState()
    LaunchedEffect(state.stage) {
        capabilityReadback = withContext(Dispatchers.IO) { readAdvancedAccess(context).toSetupReadback() }
    }
    val effectiveAccess = if (state.access != RestrictedSetupAccess.UNKNOWN) state.access else currentReadback.restrictedMessages
    val effectiveBlocked = if (state.blockedSendersAccess != RestrictedSetupAccess.UNKNOWN) state.blockedSendersAccess else currentReadback.blockedSenders
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(controller, lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) controller.clearNotice() }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    RestrictedSetupPanel(
        state = state.copy(access = effectiveAccess, blockedSendersAccess = effectiveBlocked),
        onAccessChanged = onAccessChanged,
        modifier = Modifier.testTag("restricted-setup-entry"),
        begin = controller::begin,
        submitCode = controller::submitCode,
        submitConnectPort = controller::submitConnectPort,
        showCodePromptAgain = controller::showCodePromptAgain,
        cancel = controller::cancel,
        startService = { RestrictedSetupRuntime.startService(context) },
    )
}

internal fun restrictedSetupAvailable(context: android.content.Context): Boolean = RestrictedSetupRuntime.isSupported(context)
