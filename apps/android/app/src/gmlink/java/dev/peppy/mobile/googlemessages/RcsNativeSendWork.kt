package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean

/** Runs bounded native RCS command candidates away from the Android main thread. */
internal class RcsNativeSendWork(
    private val enabled: () -> Boolean,
    private val candidates: () -> List<RcsNativeCommand>,
    private val execute: (RcsNativeCommand) -> Unit,
    private val current: () -> Boolean,
    private val executor: Executor,
    /**
     * Asked after each command: true when it left a carrier-only proof for the gateway pass. The
     * proof holds only until the next native attempt, so the batch ends there.
     */
    private val carrierHandoffRequested: () -> Boolean = { false },
) {
    private val lock = Any()
    private val stopped = AtomicBoolean(false)
    private var running = false
    private var wakePending = false

    fun start() {
        if (!enabled()) return
        val schedule = synchronized(lock) {
            if (stopped.get()) return@synchronized false
            if (running) {
                wakePending = true
                recordGoogleMessagesLinkEvent("send_work", "busy", 0)
                false
            } else {
                running = true
                true
            }
        }
        if (schedule) executor.execute(::run)
    }

    private fun run() {
        var slotReleased = false
        try {
            while (!stopped.get() && current()) {
                val commands = try {
                    candidates()
                } catch (_: Throwable) {
                    recordGoogleMessagesLinkEvent("link", "rcs_callback_failed", 0)
                    emptyList()
                }
                for (command in commands) {
                    if (stopped.get() || !current()) return
                    try {
                        execute(command)
                    } catch (_: Throwable) {
                        recordGoogleMessagesLinkEvent("link", "rcs_callback_failed", 0)
                    }
                    // Any later attempt would stale the proof, so the rest of the batch and a
                    // wake that arrived meanwhile wait for the next wake or the caller's retry.
                    if (carrierHandoffRequested()) return
                }
                // A candidate may legitimately remain pending (for example an upload retry or an
                // unknown carrier outcome). Only a fresh wake earns another enumeration.
                val repeat = synchronized(lock) {
                    if (wakePending && !stopped.get() && enabled() && current()) {
                        wakePending = false
                        true
                    } else {
                        running = false
                        false
                    }
                }
                if (!repeat) {
                    slotReleased = true
                    return
                }
            }
        } finally {
            if (!slotReleased) synchronized(lock) {
                running = false
                wakePending = false
            }
        }
    }

    fun stop() {
        stopped.set(true)
        synchronized(lock) { wakePending = false }
    }
}
