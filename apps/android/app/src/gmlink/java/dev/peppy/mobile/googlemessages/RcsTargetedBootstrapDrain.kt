package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException

/**
 * Outcome of one targeted bootstrap run, as seen by [RcsTargetedBootstrapDrain].
 *
 * Terminal refusals remove the key only when the target is definitively ineligible. Generic mapping, resolution,
 * observation, and claim refusals are transient because a later snapshot can change the answer.
 *
 * Transient results re-arm the key for the next trigger: an incomplete snapshot, a lifecycle or owned read failure,
 * a Go observation or route failure, an exception, an uncertain resolution, and a claim whose recovery is
 * incomplete or aborted.
 */
internal enum class RcsBootstrapResult {
    /** The target is owned and its targeted recovery completed. Removes the key. */
    COMPLETE,

    /** Terminal refusal. Removes the key. */
    TERMINAL_REFUSAL,

    /** Transient failure or incomplete recovery. Re-arms the key for the next trigger. */
    TRANSIENT,

    /** The service, generation or lease is gone. Stops the pass; keys wait for the next trigger. */
    STOPPED,
}

/**
 * One bounded, keyed, coalescing drain for targeted bootstrap requests.
 *
 * At most one drain task is queued or running. A task takes one key when it executes and then continues with the
 * other keys. A key
 * that receives arrivals while in flight runs once more afterwards. A transient result parks the key until the next
 * [offer] or [kick]. A pass never re-runs a parked key, so there is no spinning and no timer.
 */
internal class RcsTargetedBootstrapDrain(
    private val executor: Executor,
    private val alive: () -> Boolean,
    private val run: (googleConversationId: String) -> RcsBootstrapResult,
) {
    private class Entry {
        var armed = false
        var arrivals = true
        var transientAttempts = 0
    }

    private class Picked(val key: String, val entry: Entry)

    private val lock = Any()
    private val entries = LinkedHashMap<String, Entry>()
    private var scheduled = false

    fun offer(googleConversationId: String) {
        val schedule = synchronized(lock) {
            val existing = entries[googleConversationId]
            if (existing == null) {
                if (entries.size >= MAX_KEYS) evictOldestLocked()
                entries[googleConversationId] = Entry()
            } else {
                existing.armed = false
                existing.arrivals = true
                existing.transientAttempts = 0
            }
            claimScheduleLocked()
        }
        if (schedule) execute()
    }

    /** A trigger (ready, conversation update, restart sweep): un-parks every key and schedules one pass. */
    fun kick() {
        val schedule = synchronized(lock) {
            entries.values.forEach { it.armed = false }
            claimScheduleLocked()
        }
        if (schedule) execute()
    }

    private fun evictOldestLocked() {
        val oldest = entries.keys.firstOrNull() ?: return
        entries.remove(oldest)
        recordGoogleMessagesLinkEvent("link", "rcs_drain_evicted", 0)
    }

    private fun claimScheduleLocked(): Boolean {
        if (scheduled || entries.values.none { !it.armed }) return false
        scheduled = true
        return true
    }

    private fun execute() {
        try {
            executor.execute(::drain)
        } catch (_: RejectedExecutionException) {
            synchronized(lock) { scheduled = false }
        }
    }

    private fun drain() {
        while (true) {
            val picked = synchronized(lock) { pickLocked() } ?: return
            val result = try {
                run(picked.key)
            } catch (_: Throwable) {
                recordGoogleMessagesLinkEvent("link", "rcs_bootstrap_failed", 0)
                RcsBootstrapResult.TRANSIENT
            }
            if (!settle(picked, result)) return
        }
    }

    internal fun drainNow() = drain()

    /** Clears [scheduled] in the same critical section that finds nothing runnable, so no offer is lost. */
    private fun pickLocked(): Picked? {
        val next = if (alive()) entries.entries.firstOrNull { !it.value.armed } else null
        if (next == null) {
            scheduled = false
            return null
        }
        next.value.arrivals = false
        return Picked(next.key, next.value)
    }

    /** Returns false when the pass must stop; [scheduled] is then already cleared. */
    private fun settle(picked: Picked, result: RcsBootstrapResult): Boolean = synchronized(lock) {
        if (result == RcsBootstrapResult.STOPPED) {
            scheduled = false
            return@synchronized false
        }
        if (entries[picked.key] === picked.entry) {
            val arrived = picked.entry.arrivals
            when (result) {
                RcsBootstrapResult.COMPLETE, RcsBootstrapResult.TERMINAL_REFUSAL -> if (!arrived) entries.remove(picked.key)
                RcsBootstrapResult.TRANSIENT -> {
                    picked.entry.transientAttempts += 1
                    if (picked.entry.transientAttempts >= MAX_TRANSIENT_ATTEMPTS) entries.remove(picked.key)
                    else picked.entry.armed = !arrived
                }
                RcsBootstrapResult.STOPPED -> Unit
            }
        }
        true
    }

    private companion object {
        const val MAX_KEYS = 64
        const val MAX_TRANSIENT_ATTEMPTS = 5
    }
}
