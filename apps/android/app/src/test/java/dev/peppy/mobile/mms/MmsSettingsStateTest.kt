package dev.peppy.mobile.mms

import org.junit.Assert.*
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
        assertEquals(MmsBackfillStatus.OlderPausedServerStorage, health.backfillStatus)
    }

    @Test
    fun generatedBackfillStatusMapsProgressStatesIntoHealth() {
        fun status(
            baselineInitialized: Boolean = true,
            historyComplete: Boolean = true,
            pendingCount: ULong = 0uL,
        ) = NativeMmsBackfillStatus(
            baselineInitialized = baselineInitialized,
            historyUpperBound = "42",
            historyComplete = historyComplete,
            pause = null,
            pendingCount = pendingCount,
            quarantinedCount = 0uL,
            recheckCursor = null,
        )

        assertEquals(MmsBackfillStatus.GettingReady, mmsHealthFromNative(emptyList(), status(baselineInitialized = false), emptyList()).backfillStatus)
        assertEquals(MmsBackfillStatus.UpToDate, mmsHealthFromNative(emptyList(), status(), emptyList()).backfillStatus)
        assertEquals(MmsBackfillStatus.Copying(3uL), mmsHealthFromNative(emptyList(), status(pendingCount = 3uL), emptyList()).backfillStatus)
        assertEquals(MmsBackfillStatus.CopyingOlder, mmsHealthFromNative(emptyList(), status(historyComplete = false), emptyList()).backfillStatus)
    }

    @Test
    fun generatedBackfillStatusMapsCapacityPausesIntoHealth() {
        fun status(pause: NativeMmsBackfillPause) = NativeMmsBackfillStatus(
            baselineInitialized = true,
            historyUpperBound = "42",
            historyComplete = false,
            pause = pause,
            pendingCount = 0uL,
            quarantinedCount = 0uL,
            recheckCursor = null,
        )

        assertEquals(MmsBackfillStatus.PausedPhoneStorage, mmsHealthFromNative(emptyList(), status(NativeMmsBackfillPause.LOCAL_CAPACITY), emptyList()).backfillStatus)
        assertEquals(MmsBackfillStatus.OlderPausedHistoryLimit, mmsHealthFromNative(emptyList(), status(NativeMmsBackfillPause.RECOVERY_CAPACITY), emptyList()).backfillStatus)
        assertEquals(MmsBackfillStatus.PausedHistoryLimit, mmsHealthFromNative(emptyList(), status(NativeMmsBackfillPause.ABSOLUTE_RECOVERY_CAPACITY), emptyList()).backfillStatus)
    }

}
