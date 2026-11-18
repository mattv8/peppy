package dev.peppy.mobile.enrollment

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PassphraseSaveTrackerTest {
    @Test fun staleGenerationIsIgnoredAfterReset() {
        val tracker = PassphraseSaveTracker()
        val generation = requireNotNull(tracker.begin())

        tracker.reset()
        tracker.finish(generation, PassphraseSaveOutcome.SAVED)

        assertEquals(PassphraseSaveState.IDLE, tracker.status)
    }

    @Test fun abandonReturnsCurrentSaveToIdle() {
        val tracker = PassphraseSaveTracker()
        val generation = requireNotNull(tracker.begin())

        tracker.abandon(generation)

        assertEquals(PassphraseSaveState.IDLE, tracker.status)
    }

    @Test fun beginWhileSavingReturnsNull() {
        val tracker = PassphraseSaveTracker()

        tracker.begin()

        assertNull(tracker.begin())
    }

    @Test fun cancellationReturnsToIdle() {
        val tracker = PassphraseSaveTracker()
        val generation = requireNotNull(tracker.begin())

        tracker.finish(generation, PassphraseSaveOutcome.CANCELLED)

        assertEquals(PassphraseSaveState.IDLE, tracker.status)
    }
}
