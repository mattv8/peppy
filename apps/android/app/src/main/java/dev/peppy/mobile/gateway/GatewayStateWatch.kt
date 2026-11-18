package dev.peppy.mobile.gateway

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Least spacing between status reloads while persisted gateway state keeps changing (UX and battery: no polling). */
internal const val GATEWAY_STATUS_RELOAD_MS = 1_000L

/**
 * The too-large state shows on the ribbon only with vault keys ready. A locked phone is told to unlock instead,
 * and its Details action has no route while locked.
 */
internal fun historyTooLargeRibbonVisible(status: GatewayStatus?): Boolean =
    status?.historyTooLarge == true && status.sharedKeysReady

/**
 * Calls [reload] while the screen is STARTED, once for each persisted gateway-state change, coalesced to at most one
 * call per [GATEWAY_STATUS_RELOAD_MS]. Wake-ups come only from SharedPreferences writes: there is no timer. [reload]
 * must be a status read and nothing else (it must not enqueue gateway work).
 */
@Composable
internal fun GatewayStateReloadEffect(
    context: Context,
    beforeRegistration: suspend () -> Unit = {},
    reload: suspend () -> Unit,
) {
    val owner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val currentReload by rememberUpdatedState(reload)
    DisposableEffect(owner) {
        var watching: Job? = null
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> watching = scope.launch {
                    watchGatewayState(context, beforeRegistration) { currentReload() }
                }
                Lifecycle.Event.ON_STOP -> {
                    watching?.cancel()
                    watching = null
                }
                else -> Unit
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose {
            owner.lifecycle.removeObserver(observer)
            watching?.cancel()
        }
    }
}

private suspend fun watchGatewayState(context: Context, beforeRegistration: suspend () -> Unit, reload: suspend () -> Unit) {
    // Opening the preferences can touch disk: keep it off the main thread.
    val store = withContext(Dispatchers.IO) { GatewayStateStore(context) }
    // The flow registers before its first signal, so a write made before registration still reaches the screen.
    coalescedReloads(store.changes(beforeRegistration), GATEWAY_STATUS_RELOAD_MS, reload)
}

/**
 * Runs [reload] for each burst of [changes]: changes that arrive during a reload or its [pauseMs] pause merge into
 * one pending reload, so the reload rate never exceeds one per pause.
 */
internal suspend fun coalescedReloads(changes: Flow<Unit>, pauseMs: Long, reload: suspend () -> Unit) {
    changes.conflate().collect {
        reload()
        delay(pauseMs)
    }
}
