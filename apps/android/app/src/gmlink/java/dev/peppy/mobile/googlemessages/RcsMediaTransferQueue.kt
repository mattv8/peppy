package dev.peppy.mobile.googlemessages

import java.util.ArrayDeque
import java.util.concurrent.Executor
import java.util.concurrent.Executors

/** Serializes Google media transfers without holding the queue lock while a transfer runs. */
internal class RcsMediaTransferQueue(
    private val executor: Executor = Executors.newSingleThreadExecutor(),
) {
    private val lock = Any()
    private val pending = ArrayDeque<Pending>()
    private var active: Pending? = null
    private var drainScheduled = false
    private var stopped = false
    private var generation = 0L

    /** Coalesces identical pending/in-flight work; completed identities may be submitted again. */
    fun submit(
        identity: String,
        onAlreadyActive: (() -> Unit)? = null,
        work: (Lease) -> Unit,
    ): Boolean {
        require(identity.isNotBlank()) { "identity must not be blank" }
        val schedule: Boolean
        synchronized(lock) {
            if (stopped) return false
            if (active?.identity == identity) {
                active?.externalAfterRelease = onAlreadyActive
                return false
            }
            if (pending.any { it.identity == identity }) return false
            pending.addLast(Pending(identity, generation, work))
            schedule = scheduleDrainLocked()
        }
        if (schedule) executor.execute(::drain)
        return true
    }

    /** Cancels queued work and fences every callback belonging to this service incarnation. */
    fun stop() {
        synchronized(lock) {
            stopped = true
            generation += 1
            pending.clear()
        }
    }

    /** The durable identity currently executing, if this incarnation is still active. */
    fun current(): String? = synchronized(lock) { active?.identity?.takeUnless { stopped } }

    private fun drain() {
        val running = synchronized(lock) {
            drainScheduled = false
            if (stopped || active != null) return
            if (pending.isEmpty()) null else pending.removeFirst().also { active = it }
        } ?: return

        try {
            running.work(Lease(this, running))
        } finally {
            val next = synchronized(lock) {
                if (active === running) active = null
                val callbacks = if (!stopped && generation == running.generation) {
                    running.afterRelease + listOfNotNull(running.externalAfterRelease)
                } else emptyList()
                QueueNext(scheduleDrainLocked(), callbacks)
            }
            next.afterRelease.forEach { runCatching(it) }
            if (next.scheduleDrain) executor.execute(::drain)
        }
    }

    private fun scheduleDrainLocked(): Boolean {
        if (stopped || active != null || pending.isEmpty() || drainScheduled) return false
        drainScheduled = true
        return true
    }

    private fun current(running: Pending): Boolean = synchronized(lock) {
        !stopped && active === running && generation == running.generation
    }

    internal class Lease internal constructor(
        private val queue: RcsMediaTransferQueue,
        private val running: Pending,
    ) {
        fun isCurrent(): Boolean = queue.current(running)

        /** Publishes only while this exact transfer remains current. */
        fun publish(callback: () -> Unit): Boolean = synchronized(queue.lock) {
            if (queue.stopped || queue.active !== running || queue.generation != running.generation) return false
            callback()
            true
        }

        /** Runs after this identity leaves the queue, so a continuation cannot be coalesced away. */
        fun afterRelease(callback: () -> Unit) = synchronized(queue.lock) {
            if (!queue.stopped && queue.active === running && queue.generation == running.generation) {
                running.afterRelease += callback
            }
        }
    }

    internal data class Pending(
        val identity: String,
        val generation: Long,
        val work: (Lease) -> Unit,
    ) {
        val afterRelease = mutableListOf<() -> Unit>()
        var externalAfterRelease: (() -> Unit)? = null
    }

    private data class QueueNext(val scheduleDrain: Boolean, val afterRelease: List<() -> Unit>)
}
