package dev.peppy.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollmentOperationStateTest {
    @Test fun staleCompletionCannotFinishANewerOperation() {
        val state = EnrollmentOperationState()

        val first = state.begin()!!
        state.invalidate()
        val second = state.begin()!!

        assertFalse(state.isActive(first))
        assertTrue(state.isActive(second))
        assertFalse(state.complete(first))
        assertTrue(state.isBusy())
        assertTrue(state.complete(second))
        assertFalse(state.isBusy())
    }

    @Test fun invalidationMakesQuietPollResultsStale() {
        val state = EnrollmentOperationState()
        val poll = state.snapshot()

        state.invalidate()

        assertFalse(state.isCurrentSnapshot(poll))
        assertFalse(state.isBusy())
    }

    @Test fun activeOperationBlocksDuplicates() {
        val state = EnrollmentOperationState()
        val operation = state.begin()!!

        assertTrue(state.begin() == null)
        assertTrue(state.complete(operation))
    }
}
