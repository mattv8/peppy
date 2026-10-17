package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPause
import uniffi.peppy_mobile_bindings.NativeMmsBackfillStatus

class MmsSettingsStateTest {
    @Test
    fun ownNumberErrorsUseSafeActionableCodesRatherThanExceptionText() {
        assertEquals(
            "This number could not be saved for the selected SIM.",
            ownAddressError(MobileBindingsException.InvalidRequest()),
        )
        assertEquals(
            "The encrypted database is unavailable. Import or unlock this phone first.",
            ownAddressError(MobileBindingsException.Closed()),
        )
        val generic = ownAddressError(IllegalStateException("/private/secret database path"))
        assertEquals("Could not save the number securely. Try again.", generic)
        assertFalse(generic.contains("secret"))
    }

    @Test
    fun acquisitionHealthSeparatesDatabaseUnavailableFromNoPendingRows() {
        assertTrue(MmsHealth(databaseUnavailable = true).databaseUnavailable)
        assertFalse(MmsHealth().databaseUnavailable)
    }
    @Test
    fun generatedBackfillStatusMapsQuarantineEvidenceAndHistoryOnlyPauseIntoHealth() {
        val status = NativeMmsBackfillStatus(
            baselineInitialized = true,
            historyUpperBound = "42",
            historyComplete = false,
            pause = NativeMmsBackfillPause.REMOTE_STORAGE,
            pendingCount = 0uL,
            quarantinedCount = 3uL,
            recheckCursor = null,
        )

        val health = mmsHealthFromNative(emptyList(), status, emptyList())

        assertEquals(3L, health.quarantinedCount)
        assertEquals("History paused: vault storage capacity", health.backfillStatus)
    }

}
