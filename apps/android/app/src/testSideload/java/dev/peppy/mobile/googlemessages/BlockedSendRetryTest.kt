package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test

class BlockedSendRetryTest {
    private val scheduled = ArrayDeque<Pair<Long, () -> Unit>>()
    private val delays = mutableListOf<Long>()
    private var sendWaiting = true
    private var retries = 0
    private val retry = BlockedSendRetry(
        schedule = { delayMs, task ->
            delays += delayMs
            scheduled += delayMs to task
        },
        sendWaiting = { sendWaiting },
        retry = { retries++ },
    )

    private fun fireNext() = scheduled.removeFirst().second()

    @Test fun `a refused send arms one retry however often it is refused`() {
        repeat(3) { retry.refused() }

        assertEquals(listOf(30_000L), delays)
        fireNext()
        assertEquals(1, retries)
    }

    @Test fun `retries back off while the send stays refused and start over after progress`() {
        retry.refused()
        fireNext()
        retry.refused()
        fireNext()
        retry.progressed()
        retry.refused()

        assertEquals(listOf(30_000L, 60_000L, 30_000L), delays)
    }

    @Test fun `a retry that could not reach the send arms the next one`() {
        retry.refused()
        fireNext()
        retry.stillBlocked()

        assertEquals(listOf(30_000L, 60_000L), delays)
        fireNext()
        assertEquals(2, retries)
    }

    @Test fun `nothing is retried once no send is waiting`() {
        retry.refused()
        sendWaiting = false
        fireNext()
        retry.stillBlocked()

        assertEquals(0, retries)
        assertEquals(listOf(30_000L), delays)
    }

    @Test fun `a send that got through is not reported as still blocked`() {
        retry.refused()
        fireNext()
        retry.progressed()
        retry.stillBlocked()

        assertEquals(listOf(30_000L), delays)
    }

    @Test fun `a stopped retry neither fires nor arms again`() {
        retry.refused()
        retry.stop()
        fireNext()
        retry.refused()

        assertEquals(0, retries)
        assertEquals(listOf(30_000L), delays)
    }
}
