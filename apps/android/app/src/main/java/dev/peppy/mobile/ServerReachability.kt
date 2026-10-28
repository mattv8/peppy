package dev.peppy.mobile

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.isActive
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.selects.onTimeout
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.io.IOException
import kotlin.math.min

internal enum class ServerReachability { UNKNOWN, AVAILABLE, UNAVAILABLE }

internal data class ReachabilityBinding(val origin: String, val vaultId: String, val deviceId: String, val generation: Long)

/** In-memory, binding-fenced reachability state. It deliberately has no durable representation. */
internal class ReachabilityStore {
    private var binding: ReachabilityBinding? = null
    private val mutableState = MutableStateFlow(ServerReachability.UNKNOWN)
    val state: StateFlow<ServerReachability> = mutableState

    @Synchronized fun bind(next: ReachabilityBinding?) {
        if (binding != next) {
            binding = next
            mutableState.value = ServerReachability.UNKNOWN
        }
    }

    @Synchronized fun record(next: ReachabilityBinding, result: ServerReachability): Boolean {
        if (binding != next) return false
        val recovered = mutableState.value == ServerReachability.UNAVAILABLE && result == ServerReachability.AVAILABLE
        mutableState.value = result
        return recovered
    }

    @Synchronized fun clear() = bind(null)
}

/** Process-owned reachability for enrolled server sync and foreground probes only. */
internal object ServerReachabilityState {
    private val store = ReachabilityStore()
    val state: StateFlow<ServerReachability> = store.state

    fun bind(binding: GatewayReachabilityBinding?) {
        store.bind(binding?.key())
    }

    fun clear() = store.clear()

    fun recordResponse(binding: GatewayReachabilityBinding, response: HttpResult): Boolean {
        return store.record(binding.key(), if (response.code in UNAVAILABLE_HTTP_CODES) ServerReachability.UNAVAILABLE else ServerReachability.AVAILABLE)
    }

    fun recordNetworkAbsent(binding: GatewayReachabilityBinding) { store.record(binding.key(), ServerReachability.UNAVAILABLE) }

    /** Only a real connection failure is unavailable; local budgets and auth are not connectivity. */
    fun recordFailure(binding: GatewayReachabilityBinding, error: Exception) {
        if (isReachabilityFailure(error)) {
            store.record(binding.key(), ServerReachability.UNAVAILABLE)
        }
    }

    private fun GatewayReachabilityBinding.key() = ReachabilityBinding(origin, vaultId, deviceId, generation)
    private val UNAVAILABLE_HTTP_CODES = setOf(502, 503, 504)
}

internal const val INITIAL_PROBE_DELAY_MS = 30_000L
internal const val MAX_PROBE_DELAY_MS = 5 * 60_000L
internal fun nextProbeDelay(delayMillis: Long) = min(delayMillis * 2, MAX_PROBE_DELAY_MS)

/** Collapses callback bursts into one probe that can interrupt the normal retry delay. */
internal class RecoveryProbeRequests {
    private val signals = Channel<Unit>(Channel.CONFLATED)
    private var pending = false

    @Synchronized fun request(): Boolean {
        if (pending) return false
        pending = true
        signals.trySend(Unit)
        return true
    }

    @OptIn(ExperimentalCoroutinesApi::class)
    suspend fun waitForNext(delayMillis: Long): Boolean = select {
        signals.onReceive {
            synchronized(this@RecoveryProbeRequests) { pending = false }
            true
        }
        onTimeout(delayMillis) { false }
    }
}

internal fun isReachabilityFailure(error: Exception): Boolean {
    if (error is CancellationException) throw error
    return error is IOException && error !is GatewayTransportException
}

@Composable
internal fun ObserveServerReachability(enrolled: Boolean, bindingRefresh: Int) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val reachability by ServerReachabilityState.state.collectAsState()
    var foreground by remember(lifecycleOwner) { mutableStateOf(lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) }
    val recoveryProbes = remember { RecoveryProbeRequests() }

    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, _ ->
            foreground = lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(enrolled, foreground, bindingRefresh) {
        ServerReachabilityState.bind(if (enrolled) withContext(Dispatchers.IO) { NativeGateway.reachabilityBinding(context) } else null)
    }

    if (!enrolled || !foreground) return
    LaunchedEffect(reachability, foreground, enrolled) {
        val binding = withContext(Dispatchers.IO) { NativeGateway.reachabilityBinding(context) } ?: return@LaunchedEffect
        ServerReachabilityState.bind(binding)
        var delayMillis = INITIAL_PROBE_DELAY_MS
        do {
            if (hasNetwork(context)) probeServer(context, binding) else ServerReachabilityState.recordNetworkAbsent(binding)
            if (ServerReachabilityState.state.value != ServerReachability.UNAVAILABLE) break
            if (!recoveryProbes.waitForNext(delayMillis)) delayMillis = nextProbeDelay(delayMillis)
        } while (isActive)
    }

    DisposableEffect(context, enrolled, foreground) {
        if (!enrolled || !foreground) return@DisposableEffect onDispose {}
        val manager = context.getSystemService(ConnectivityManager::class.java)
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onLost(network: Network) {
                if (!hasNetwork(context)) NativeGateway.reachabilityBinding(context)?.let(ServerReachabilityState::recordNetworkAbsent)
            }

            override fun onAvailable(network: Network) {
                if (ServerReachabilityState.state.value == ServerReachability.UNAVAILABLE && hasNetwork(context)) {
                    recoveryProbes.request()
                }
            }
        }
        manager.registerNetworkCallback(NetworkRequest.Builder().addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET).build(), callback)
        onDispose { manager.unregisterNetworkCallback(callback) }
    }
}

private val probeMutex = Mutex()

private suspend fun probeServer(context: Context, binding: GatewayReachabilityBinding) = probeMutex.withLock {
    if (withContext(Dispatchers.IO) { NativeGateway.reachabilityBinding(context) }?.sameBinding(binding) != true) return
    try {
        val response = withContext(Dispatchers.IO) { GatewayHttp(binding.origin, binding.bearerToken).get("/v1/events?after=0&limit=1") }
        if (ServerReachabilityState.recordResponse(binding, response)) GatewayWork.enqueue(context)
    } catch (error: Exception) {
        ServerReachabilityState.recordFailure(binding, error)
    }
}

private fun GatewayReachabilityBinding.sameBinding(other: GatewayReachabilityBinding) =
    origin == other.origin && vaultId == other.vaultId && deviceId == other.deviceId && generation == other.generation

private fun hasNetwork(context: Context): Boolean {
    val manager = context.getSystemService(ConnectivityManager::class.java)
    val network = manager.activeNetwork ?: return false
    return manager.getNetworkCapabilities(network)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
}
