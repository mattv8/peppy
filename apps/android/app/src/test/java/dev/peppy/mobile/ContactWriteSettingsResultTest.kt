package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ContactWriteSettingsResultTest {
    private val awaiting = ConsentGatedToggle(ConsentPhase.AWAITING_ACCESS)

    @Test fun grantPreservesConfirmModeAndStartsApplying() {
        val result = contactWriteSettingsResult(awaiting, "confirm", granted = true, pending = true)
        assertEquals("confirm", result.modeToApply)
        assertEquals(ConsentPhase.APPLYING, result.state.phase)
    }

    @Test fun grantPreservesAutoModeAndStartsApplying() {
        val result = contactWriteSettingsResult(awaiting, "auto", granted = true, pending = true)
        assertEquals("auto", result.modeToApply)
        assertEquals(ConsentPhase.APPLYING, result.state.phase)
    }

    @Test fun denialReturnsToIdleWithoutApply() {
        val result = contactWriteSettingsResult(awaiting, "auto", granted = false, pending = true)
        assertNull(result.modeToApply)
        assertEquals(ConsentPhase.IDLE, result.state.phase)
    }

    @Test fun cancelledConsentIsNotRevivedByReturn() {
        val result = contactWriteSettingsResult(ConsentGatedToggle(), "auto", granted = true, pending = true)
        assertNull(result.modeToApply)
        assertEquals(ConsentPhase.IDLE, result.state.phase)
    }

    @Test fun staleReturnWithoutPendingIntentReturnsToIdleWithoutApply() {
        val result = contactWriteSettingsResult(awaiting, "auto", granted = true, pending = false)
        assertNull(result.modeToApply)
        assertEquals(ConsentPhase.IDLE, result.state.phase)
    }

    @Test fun grantWithMissingModeReturnsToIdleWithoutApply() {
        val result = contactWriteSettingsResult(awaiting, null, granted = true, pending = true)
        assertNull(result.modeToApply)
        assertEquals(ConsentPhase.IDLE, result.state.phase)
    }

    @Test fun resultIsConsumedOnlyOnce() {
        val first = contactWriteSettingsResult(awaiting, "auto", granted = true, pending = true)
        val second = contactWriteSettingsResult(first.state, "auto", granted = true, pending = true)
        assertNull(second.modeToApply)
        assertEquals(ConsentPhase.APPLYING, second.state.phase)
    }
}
