package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test

class CarrierProofWakeTrackerTest {
    @Test fun `carrier only wakes exactly three times then waits for normal cadence`() {
        val tracker = CarrierProofWakeTracker()

        assertEquals(CarrierProofWakeAction.WAKE, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.WAKE, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.WAKE, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.PROOF_WINDOW_MISSED, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.PROOF_WINDOW_MISSED, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
    }

    @Test fun `other candidate pages do not reset an existing proof wake count`() {
        val tracker = CarrierProofWakeTracker()
        repeat(3) { tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY) }
        tracker.record("another-command", RcsNativeSendOutcome.CARRIER_ONLY)

        assertEquals(CarrierProofWakeAction.PROOF_WINDOW_MISSED, tracker.record("command", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.NONE, tracker.record("command", RcsNativeSendOutcome.SUBMITTED))
    }

    @Test fun `capacity exhaustion cannot evict and rearm an existing fuse`() {
        val tracker = CarrierProofWakeTracker()
        repeat(128) { command ->
            repeat(3) { tracker.record("command-$command", RcsNativeSendOutcome.CARRIER_ONLY) }
        }

        assertEquals(CarrierProofWakeAction.PROOF_WINDOW_MISSED, tracker.record("overflow", RcsNativeSendOutcome.CARRIER_ONLY))
        assertEquals(CarrierProofWakeAction.PROOF_WINDOW_MISSED, tracker.record("command-0", RcsNativeSendOutcome.CARRIER_ONLY))
        tracker.record("command-0", RcsNativeSendOutcome.SUBMITTED)
        assertEquals(CarrierProofWakeAction.WAKE, tracker.record("overflow", RcsNativeSendOutcome.CARRIER_ONLY))
    }
}
