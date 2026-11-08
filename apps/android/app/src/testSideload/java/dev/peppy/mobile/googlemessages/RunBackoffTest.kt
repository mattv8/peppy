package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test

class RunBackoffTest {
    @Test
    fun `backs off from thirty seconds to ten minutes`() {
        val backoff = RunBackoff()

        assertEquals(listOf(30_000L, 60_000L, 120_000L, 300_000L, 600_000L, 600_000L), List(6) { backoff.nextDelayMillis() })
    }

    @Test
    fun `ready resets the next delay`() {
        val backoff = RunBackoff()
        backoff.nextDelayMillis()
        backoff.nextDelayMillis()

        backoff.reset()

        assertEquals(30_000L, backoff.nextDelayMillis())
    }
}
