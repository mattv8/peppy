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
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.mms.restrictedMessageAccess
import dev.peppy.mobile.restrictedsetup.ui.RestrictedSetupPanel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
internal fun RestrictedSetupEntry(onAccessChanged: () -> Unit) {
    val context = LocalContext.current
    if (!RestrictedSetupRuntime.isSupported(context)) return
    var readback by remember { mutableStateOf<RestrictedMessageAccess?>(null) }
    LaunchedEffect(Unit) { readback = withContext(Dispatchers.IO) { restrictedMessageAccess(context) } }
    val initialReadback = readback ?: return
    if (initialReadback == RestrictedMessageAccess.UNSUPPORTED) return
    val controller = RestrictedSetupRuntime.controller(context)
    val state by controller.state.collectAsState()
    LaunchedEffect(state.stage) { readback = withContext(Dispatchers.IO) { restrictedMessageAccess(context) } }
    val controllerAccess = state.access
    val effectiveAccess = if (controllerAccess != RestrictedSetupAccess.UNKNOWN) controllerAccess else when (readback) {
        RestrictedMessageAccess.ALLOWED -> RestrictedSetupAccess.ALLOWED
        RestrictedMessageAccess.NOT_ALLOWED -> RestrictedSetupAccess.NOT_ALLOWED
        else -> RestrictedSetupAccess.UNKNOWN
    }
    if (readback == RestrictedMessageAccess.UNSUPPORTED) return
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(controller, lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) controller.clearNotice() }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    RestrictedSetupPanel(
        state = state.copy(access = effectiveAccess),
        onAccessChanged = onAccessChanged,
        modifier = Modifier.testTag("restricted-setup-entry"),
        begin = controller::begin,
        submitCode = controller::submitCode,
        submitConnectPort = controller::submitConnectPort,
        showCodePromptAgain = controller::showCodePromptAgain,
        cancel = controller::cancel,
        acknowledgeCleanup = controller::acknowledgeCleanup,
        startService = { RestrictedSetupRuntime.startService(context) },
    )
}
