package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneReadSettingsStateTest {
    @Test
    fun loadingAndUnavailableStatesCannotChangeTheSetting() {
        assertFalse(phoneReadPresentation(enabled = null, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, supported = true, loading = true).toggleEnabled)
        assertEquals(PhoneReadPresentation.State.UNAVAILABLE, phoneReadPresentation(false, true, true, true, false).state)
        assertFalse(phoneReadPresentation(false, true, true, true, false).toggleEnabled)
    }

    @Test
    fun disabledPhoneReadRequiresSmsSyncBeforeItCanBeEnabled() {
        val paused = phoneReadPresentation(enabled = false, smsSyncEnabled = false, smsReadAccess = true, notificationAccess = false, supported = true)

        assertEquals(PhoneReadPresentation.State.SMS_SYNC_REQUIRED, paused.state)
        assertFalse(paused.toggleEnabled)
    }

    @Test
    fun enabledPhoneReadStaysSwitchableOffWhenAccessOrSmsSyncIsMissing() {
        val accessPaused = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, supported = true)
        val smsPaused = phoneReadPresentation(enabled = true, smsSyncEnabled = false, smsReadAccess = true, notificationAccess = false, supported = true)

        assertEquals(PhoneReadPresentation.State.ACCESS_PAUSED, accessPaused.state)
        assertTrue(accessPaused.toggleEnabled)
        assertTrue(accessPaused.showOpenAccess)
        assertEquals(PhoneReadPresentation.State.SMS_SYNC_PAUSED, smsPaused.state)
        assertTrue(smsPaused.toggleEnabled)
        assertTrue(smsPaused.showOpenAccess)
    }

    @Test
    fun smsReadAccessGatesEnableAndPausesAnAlreadyEnabledSetting() {
        val disabled = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = false, notificationAccess = true, supported = true)
        val enabled = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = false, notificationAccess = true, supported = true)

        assertEquals(PhoneReadPresentation.State.SMS_READ_REQUIRED, disabled.state)
        assertFalse(disabled.toggleEnabled)
        assertEquals(PhoneReadPresentation.State.SMS_READ_PAUSED, enabled.state)
        assertTrue(enabled.toggleEnabled)
    }

    @Test
    fun failedOrMismatchedReadbackKeepsTheLastKnownStateForRetry() {
        assertEquals(PhoneReadReadback(true, true), phoneReadReadback(lastKnown = true, requested = false, readBack = null))
        assertEquals(PhoneReadReadback(false, true), phoneReadReadback(lastKnown = false, requested = true, readBack = false))
        assertEquals(PhoneReadReadback(true, false), phoneReadReadback(lastKnown = false, requested = true, readBack = true))
    }

    @Test
    fun backgroundRefreshKeepsTheKnownSwitchStateAndActionable() {
        val refreshingOn = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, supported = true, loading = true)
        val refreshingOff = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, supported = true, loading = true)

        assertEquals(PhoneReadPresentation.State.WORKING, refreshingOn.state)
        assertTrue(refreshingOn.toggleEnabled)
        assertEquals(PhoneReadPresentation.State.OFF_READY, refreshingOff.state)
        assertTrue(refreshingOff.toggleEnabled)
    }

    @Test
    fun savingIsTheOnlyKnownValueStateThatDisablesTheSwitch() {
        val saving = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, supported = true, saving = true)

        assertEquals(PhoneReadPresentation.State.SAVING, saving.state)
        assertFalse(saving.toggleEnabled)
    }

    @Test
    fun deniedAccessKeepsTheOffSettingActionableForANewAttempt() {
        val denied = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, supported = true, accessDenied = true)

        assertEquals(PhoneReadPresentation.State.ACCESS_DENIED, denied.state)
        assertTrue(denied.toggleEnabled)
        assertTrue(denied.showOpenAccess)
    }

    @Test
    fun cancelledOrDeniedConsentCannotApplyAfterALaterGrant() {
        val pending = ConsentGatedToggle().requestEnable().agree(hasAccess = false).state

        assertEquals(ConsentEffect.NONE, pending.accessResult(granted = false).state.accessResult(granted = true).effect)
        assertEquals(ConsentEffect.NONE, pending.cancel().accessResult(granted = true).effect)
    }

    @Test
    fun deniedAccessShowsFeedbackButCannotEnableUntilANewConsentFlow() {
        val pending = ConsentGatedToggle().requestEnable().agree(hasAccess = false).state

        val denied = phoneReadAccessResult(pending, granted = false)

        assertTrue(denied.denied)
        assertFalse(denied.apply)
        assertFalse(phoneReadAccessResult(denied.consent, granted = true).apply)
    }

    @Test
    fun staleRefreshCannotOverwriteANewerSave() {
        assertFalse(phoneReadOperationIsCurrent(operation = 4, current = 5))
        assertTrue(phoneReadOperationIsCurrent(operation = 5, current = 5))
    }
}
