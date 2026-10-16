package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class SmsSettingsStateTest {
    @Test
    fun historyStateDoesNotClaimCaptureUntilTheCoreReportsIt() {
        assertEquals(SmsHistoryState.WAITING_FOR_PERMISSION, smsHistoryState(false, true, true))
        assertEquals(SmsHistoryState.SYNC_OFF, smsHistoryState(true, false, true))
        assertEquals(SmsHistoryState.WAITING_FOR_VAULT, smsHistoryState(true, true, false))
        assertEquals(SmsHistoryState.WAITING, smsHistoryState(true, true, true))
    }

    @Test
    fun coreStatusPrioritizesProviderErrorsAndUnresolvedRows() {
        assertEquals(
            SmsHistoryState.UNAVAILABLE,
            smsHistoryState(SmsHistoryCoreStatus(true, true, true, true, true, true)),
        )
        assertEquals(
            SmsHistoryState.PENDING,
            smsHistoryState(SmsHistoryCoreStatus(true, false, true, false, false, true)),
        )
    }

    @Test
    fun coreStatusMapsPausedReadingAndCompleteWithoutGuessing() {
        assertEquals(SmsHistoryState.PAUSED, smsHistoryState(SmsHistoryCoreStatus(true, true, false, true, false, false)))
        assertEquals(SmsHistoryState.READING, smsHistoryState(SmsHistoryCoreStatus(true, false, false, false, false, false)))
        assertEquals(SmsHistoryState.CAPTURED, smsHistoryState(SmsHistoryCoreStatus(true, false, true, false, false, false)))
    }

    @Test
    fun permissionRecoveryOnlyOpensSettingsAfterAPermanentDenial() {
        assertEquals(false, needsSmsReadSettingsRecovery(false, false, false))
        assertEquals(false, needsSmsReadSettingsRecovery(true, false, true))
        assertEquals(true, needsSmsReadSettingsRecovery(true, false, false))
        assertEquals(false, needsSmsReadSettingsRecovery(true, true, false))
    }
}
