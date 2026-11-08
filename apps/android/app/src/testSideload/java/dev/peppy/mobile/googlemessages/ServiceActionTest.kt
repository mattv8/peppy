package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test

class ServiceActionTest {
    @Test
    fun `terminal states stop the service`() {
        listOf("unlinked", "signed_out", "needs_relink").forEach { state ->
            assertEquals(ServiceAction.STOP, nextServiceAction(status(state), pairingInProgress = false, running = false))
        }
    }

    @Test
    fun `linked but not running starts run`() {
        assertEquals(ServiceAction.RUN, nextServiceAction(status("linked"), pairingInProgress = false, running = false))
    }

    @Test
    fun `pairing waits for the owner flow`() {
        assertEquals(ServiceAction.WAIT, nextServiceAction(status("pairing"), pairingInProgress = false, running = false))
        assertEquals(ServiceAction.WAIT, nextServiceAction(status("unlinked"), pairingInProgress = true, running = false))
    }

    private fun status(state: String) = LinkStatus(0, state, false, false, false, 0, "")
}
