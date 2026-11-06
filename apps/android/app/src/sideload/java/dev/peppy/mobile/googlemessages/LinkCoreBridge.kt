package dev.peppy.mobile.googlemessages

import android.content.Context
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.Executors
import uniffi.peppy_mobile_bindings.NativeRcsLinkState

/** Records content-free link lifecycle on a worker thread before linked status is surfaced. */
internal object LinkCoreBridge {
    @Volatile var recorder: ((Context, LinkStatus) -> Boolean)? = null
    private var lastState: String? = null
    private var lastRecordedAt = 0L
    private var preStartGeneration = 0L
    private val lifecycleExecutor = Executors.newSingleThreadExecutor()

    /** Called only from link worker threads; suppresses duplicate lifecycle observations for ten minutes. */
    fun recordIfNeeded(context: Context, status: LinkStatus): Boolean = lifecycleExecutor.submit<Boolean> {
        synchronized(this) {
            val now = System.currentTimeMillis()
            if (status.state == lastState && now - lastRecordedAt < HEARTBEAT_MS) return@synchronized true
            if (!record(context, status)) return@synchronized false
            lastState = status.state
            lastRecordedAt = now
            true
        }
    }.get()

    fun recordPreStart(context: Context): Boolean {
        synchronized(this) { preStartGeneration += 1 }
        LinkCoreBridgeTestHooks.preStartFenceBegun?.invoke()
        return lifecycleExecutor.submit<Boolean> {
        synchronized(this) {
            val status = LinkStatus(0, "unknown", false, false, false, 0, "")
            if (!record(context, status)) return@synchronized false
            lastState = status.state
            lastRecordedAt = System.currentTimeMillis()
            true
        }
        }.get()
    }

    fun observationToken(): Long = synchronized(this) { preStartGeneration }

    fun recordObservation(token: Long, context: Context, status: LinkStatus): Boolean {
        return lifecycleExecutor.submit<Boolean> {
            LinkCoreBridgeTestHooks.beforeObservationValidation?.invoke()
            synchronized(this) {
                if (LinkRuntime.pairingInProgress.get() || token != preStartGeneration) return@synchronized true
                recordIfNeededNow(context, status)
            }
        }.get()
    }

    internal fun resetForTest() = synchronized(this) {
        recorder = null
        lastState = null
        lastRecordedAt = 0L
        preStartGeneration = 0L
        LinkCoreBridgeTestHooks.beforeObservationValidation = null
        LinkCoreBridgeTestHooks.preStartFenceBegun = null
    }

    fun record(context: Context, status: LinkStatus): Boolean {
        recorder?.let { return it(context, status) }
        val state = when (status.state) {
            "linked" -> NativeRcsLinkState.LINKED
            "unlinked" -> NativeRcsLinkState.UNLINKED
            "needs_relink" -> NativeRcsLinkState.NEEDS_RELINK
            "signed_out" -> NativeRcsLinkState.SIGNED_OUT
            else -> NativeRcsLinkState.UNAVAILABLE
        }
        return runCatching {
            val now = System.currentTimeMillis()
            NativeGateway.open(context.applicationContext)?.recordRcsLinkLifecycle(
                state,
                status.state != "unknown",
                status.healthy,
                status.phoneResponding,
                now,
                now,
            ) != null
        }.getOrDefault(false)
    }

    private fun recordIfNeededNow(context: Context, status: LinkStatus): Boolean {
        val now = System.currentTimeMillis()
        if (status.state == lastState && now - lastRecordedAt < HEARTBEAT_MS) return true
        if (!record(context, status)) return false
        lastState = status.state
        lastRecordedAt = now
        return true
    }

    private const val HEARTBEAT_MS = 10 * 60 * 1000L
}

internal object LinkCoreBridgeTestHooks {
    @Volatile var beforeObservationValidation: (() -> Unit)? = null
    @Volatile var preStartFenceBegun: (() -> Unit)? = null
}
