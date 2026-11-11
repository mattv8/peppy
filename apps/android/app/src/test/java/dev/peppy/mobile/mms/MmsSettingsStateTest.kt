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
    fun presentationSeparatesDisabledMmsFromMissingReceiveAccess() {
        assertEquals(
            MmsPresentationStatus.OFF,
            mmsHealthPresentation(enabled = false, receiveGranted = true, health = MmsHealth()).status,
        )
        assertEquals(
            MmsPresentationStatus.ACCESS_NEEDED,
            mmsHealthPresentation(enabled = true, receiveGranted = false, health = MmsHealth()).status,
        )
    }

    @Test
    fun presentationPrioritizesLockedAndKeepsPausedHistoryVariantsDistinct() {
        assertEquals(
            MmsPresentationStatus.LOCKED,
            mmsHealthPresentation(true, true, MmsHealth(databaseUnavailable = true)).status,
        )
        assertEquals(
            MmsPresentationStatus.OLDER_HISTORY_PAUSED,
            mmsHealthPresentation(true, true, MmsHealth(backfillStatus = MmsBackfillStatus.OlderPausedHistoryLimit)).status,
        )
        assertEquals(
            MmsPresentationStatus.HISTORY_PAUSED,
            mmsHealthPresentation(true, true, MmsHealth(backfillStatus = MmsBackfillStatus.PausedHistoryLimit)).status,
        )
    }

    @Test
    fun presentationShowsCopyingWithoutAProblemNote() {
        val presentation = mmsHealthPresentation(
            true,
            true,
            MmsHealth(pending = 3, backfillStatus = MmsBackfillStatus.Copying(3uL)),
        )

        assertEquals(MmsPresentationStatus.COPYING, presentation.status)
        assertEquals(null, presentation.note)
        assertFalse(presentation.showRescan)
    }

    @Test
    fun pendingAcquisitionsStayCopyingWhenBackfillLooksCurrent() {
        val presentation = mmsHealthPresentation(
            true,
            true,
            MmsHealth(pending = 2, backfillStatus = MmsBackfillStatus.UpToDate),
        )

        assertEquals(MmsPresentationStatus.COPYING, presentation.status)
        assertEquals(null, presentation.note)
        assertFalse(presentation.showRescan)
    }

    @Test
    fun unknownBackfillStatusIsGettingReadyRatherThanUpToDate() {
        assertEquals(MmsPresentationStatus.GETTING_READY, mmsHealthPresentation(true, true, MmsHealth()).status)
        assertEquals(
            MmsPresentationStatus.GETTING_READY,
            mmsHealthPresentation(true, true, MmsHealth(backfillStatus = null)).status,
        )
    }

    @Test
    fun pendingAcquisitionsDoNotOverrideOffAccessLockedOrPausePrecedence() {
        val pending = MmsHealth(pending = 2, backfillStatus = MmsBackfillStatus.UpToDate)

        val off = mmsHealthPresentation(false, true, pending)
        assertEquals(MmsPresentationStatus.OFF, off.status)
        assertEquals(null, off.note)

        assertEquals(MmsPresentationStatus.ACCESS_NEEDED, mmsHealthPresentation(true, false, pending).status)
        assertEquals(
            MmsPresentationStatus.LOCKED,
            mmsHealthPresentation(true, true, pending.copy(databaseUnavailable = true)).status,
        )
        listOf(
            MmsBackfillStatus.PausedPhoneStorage to MmsPresentationStatus.PHONE_STORAGE_PAUSED,
            MmsBackfillStatus.OlderPausedHistoryLimit to MmsPresentationStatus.OLDER_HISTORY_PAUSED,
            MmsBackfillStatus.PausedHistoryLimit to MmsPresentationStatus.HISTORY_PAUSED,
            MmsBackfillStatus.OlderPausedServerStorage to MmsPresentationStatus.SERVER_STORAGE_PAUSED,
        ).forEach { (backfill, expected) ->
            assertEquals(expected, mmsHealthPresentation(true, true, MmsHealth(pending = 2, backfillStatus = backfill)).status)
        }
    }

    @Test
    fun missingReceiveAccessGivesGuidanceNoteButIsNotARecoveryTrigger() {
        val access = mmsHealthPresentation(true, false, MmsHealth(blocked = 1, unavailable = 1))

        assertEquals(MmsPresentationStatus.ACCESS_NEEDED, access.status)
        assertEquals("ACCESS_NEEDED", access.note?.name)
        assertFalse(access.showRescan)
    }

    @Test
    fun disabledMmsHasNoGuidanceNoteEvenWithIssues() {
        val disabled = mmsHealthPresentation(false, true, MmsHealth(blocked = 1, transferReasons = listOf("upload: auth")))

        assertEquals(MmsPresentationStatus.OFF, disabled.status)
        assertEquals(null, disabled.note)
        assertFalse(disabled.showRescan)
    }

    @Test
    fun realTransferFailureStringsNoteOnlyNonTransientReasons() {
        listOf("upload: transient", "download: transient").forEach { reason ->
            val presentation = mmsHealthPresentation(
                true,
                true,
                MmsHealth(transferReasons = listOf(reason), backfillStatus = MmsBackfillStatus.UpToDate),
            )
            assertEquals(reason, MmsPresentationStatus.UP_TO_DATE, presentation.status)
            assertEquals(reason, null, presentation.note)
            assertFalse(reason, presentation.showRescan)
        }

        listOf("upload: auth", "download: quota", "upload: permanent", "download: invalid_media").forEach { reason ->
            val presentation = mmsHealthPresentation(
                true,
                true,
                MmsHealth(transferReasons = listOf(reason), backfillStatus = MmsBackfillStatus.UpToDate),
            )
            assertEquals(reason, MmsPresentationStatus.MOSTLY_UP_TO_DATE, presentation.status)
            assertEquals(reason, MmsHealthNote.NOT_COPIED, presentation.note)
            assertFalse(reason, presentation.showRescan)
        }
    }

    @Test
    fun presentationShowsOneIssueNoteAndOnlyRescansRecoverableAcquisitionFailures() {
        val transferOnly = mmsHealthPresentation(
            true,
            true,
            MmsHealth(transferReasons = listOf("upload: permanent"), backfillStatus = MmsBackfillStatus.UpToDate),
        )
        assertEquals(MmsPresentationStatus.MOSTLY_UP_TO_DATE, transferOnly.status)
        assertEquals(MmsHealthNote.NOT_COPIED, transferOnly.note)
        assertFalse(transferOnly.showRescan)

        val unavailable = mmsHealthPresentation(
            true,
            true,
            MmsHealth(unavailable = 1, quarantinedCount = 1, backfillStatus = MmsBackfillStatus.UpToDate),
        )
        assertEquals(MmsHealthNote.BOTH, unavailable.note)
        assertTrue(unavailable.showRescan)

        assertTrue(mmsHealthPresentation(true, true, MmsHealth(blocked = 1)).showRescan)
    }

    @Test
    fun presentationDoesNotOfferRecoveryForWaitingOnlyOrWhenLockedOrOff() {
        assertFalse(mmsHealthPresentation(true, true, MmsHealth(pending = 1)).showRescan)
        assertFalse(mmsHealthPresentation(true, true, MmsHealth(databaseUnavailable = true, unavailable = 1)).showRescan)
        assertFalse(mmsHealthPresentation(false, true, MmsHealth(unavailable = 1)).showRescan)
    }

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
