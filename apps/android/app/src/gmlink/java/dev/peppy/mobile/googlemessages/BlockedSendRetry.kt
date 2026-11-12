package dev.peppy.mobile.googlemessages

/**
 * Retries a native send that was refused before it had any effect. Such a send is otherwise retried
 * only by the next link event or gateway pass, so one delayed retry stays armed while it waits.
 */
internal class BlockedSendRetry(
    private val schedule: (delayMs: Long, task: () -> Unit) -> Unit,
    private val sendWaiting: () -> Boolean,
    private val retry: () -> Unit,
) {
    private val lock = Any()
    private val backoff = RunBackoff()
    private var blocked = false
    private var armed = false
    private var stopped = false

    /** A send was refused and is still waiting. */
    fun refused() = arm(newlyBlocked = true)

    /** A retry could not reach the send, for example because the index refresh failed. */
    fun stillBlocked() = arm(newlyBlocked = false)

    /** A send got through, so the next refusal starts from the shortest delay. */
    fun progressed() = synchronized(lock) {
        blocked = false
        backoff.reset()
    }

    fun stop() = synchronized(lock) { stopped = true }

    private fun arm(newlyBlocked: Boolean) {
        val delayMs = synchronized(lock) {
            if (newlyBlocked) blocked = true
            if (stopped || armed || !blocked) return
            armed = true
            backoff.nextDelayMillis()
        }
        schedule(delayMs, ::fire)
    }

    private fun fire() {
        synchronized(lock) {
            armed = false
            if (stopped) return
        }
        val waiting = sendWaiting()
        synchronized(lock) {
            blocked = waiting
            if (!waiting) backoff.reset()
        }
        if (waiting) retry()
    }
}
