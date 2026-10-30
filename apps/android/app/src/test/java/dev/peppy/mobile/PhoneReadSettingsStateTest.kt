package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneReadSettingsStateTest {
    @Test
    fun loadingStateCannotChangeTheSetting() {
        assertFalse(phoneReadPresentation(enabled = null, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, outboundSupported = true, loading = true).toggleEnabled)
    }

    @Test
    fun disabledPhoneReadRequiresSmsSyncBeforeItCanBeEnabled() {
        val paused = phoneReadPresentation(enabled = false, smsSyncEnabled = false, smsReadAccess = true, notificationAccess = false, outboundSupported = true)

        assertEquals(PhoneReadPresentation.State.SMS_SYNC_REQUIRED, paused.state)
        assertFalse(paused.toggleEnabled)
    }

    @Test
    fun enabledPhoneReadStaysSwitchableOffWhenAccessOrSmsSyncIsMissing() {
        val accessPaused = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, outboundSupported = true)
        val smsPaused = phoneReadPresentation(enabled = true, smsSyncEnabled = false, smsReadAccess = true, notificationAccess = false, outboundSupported = true)

        assertEquals(PhoneReadPresentation.State.ACCESS_LIMITED, accessPaused.state)
        assertTrue(accessPaused.toggleEnabled)
        assertTrue(accessPaused.showOpenAccess)
        assertEquals(PhoneReadPresentation.State.SMS_SYNC_PAUSED, smsPaused.state)
        assertTrue(smsPaused.toggleEnabled)
        assertTrue(smsPaused.showOpenAccess)
    }

    @Test
    fun smsReadAccessGatesEnableAndPausesAnAlreadyEnabledSetting() {
        val disabled = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = false, notificationAccess = true, outboundSupported = true)
        val enabled = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = false, notificationAccess = true, outboundSupported = true)

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
        val refreshingOn = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, outboundSupported = true, loading = true)
        val refreshingOff = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, outboundSupported = true, loading = true)

        assertEquals(PhoneReadPresentation.State.WORKING, refreshingOn.state)
        assertTrue(refreshingOn.toggleEnabled)
        assertEquals(PhoneReadPresentation.State.OFF_READY, refreshingOff.state)
        assertTrue(refreshingOff.toggleEnabled)
    }

    @Test
    fun savingIsTheOnlyKnownValueStateThatDisablesTheSwitch() {
        val saving = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = true, outboundSupported = true, saving = true)

        assertEquals(PhoneReadPresentation.State.SAVING, saving.state)
        assertFalse(saving.toggleEnabled)
    }

    @Test
    fun notificationAccessDoesNotBlockEnablement() {
        val denied = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, outboundSupported = true, accessDenied = true)

        assertEquals(PhoneReadPresentation.State.OFF_READY, denied.state)
        assertTrue(denied.toggleEnabled)
        assertFalse(denied.showOpenAccess)
    }

    @Test
    fun agreeAppliesImmediatelyWithoutNotificationAccess() {
        val transition = ConsentGatedToggle().requestEnable().agree(hasAccess = true)

        assertEquals(ConsentEffect.APPLY, transition.effect)
    }

    @Test
    fun inboundSyncWorksWithoutAndroid9OrNotificationAccess() {
        val off = phoneReadPresentation(enabled = false, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, outboundSupported = false)
        val on = phoneReadPresentation(enabled = true, smsSyncEnabled = true, smsReadAccess = true, notificationAccess = false, outboundSupported = false)

        assertEquals(PhoneReadPresentation.State.OFF_READY, off.state)
        assertTrue(off.toggleEnabled)
        assertEquals(PhoneReadPresentation.State.OUTBOUND_UNSUPPORTED, on.state)
        assertTrue(on.toggleEnabled)
        assertFalse(on.showOpenAccess)
    }

    @Test
    fun unsupportedOutboundDoesNotOfferNotificationAccessWhileSmsSyncIsPaused() {
        val paused = phoneReadPresentation(enabled = true, smsSyncEnabled = false, smsReadAccess = true, notificationAccess = false, outboundSupported = false)

        assertEquals(PhoneReadPresentation.State.SMS_SYNC_PAUSED, paused.state)
        assertTrue(paused.toggleEnabled)
        assertFalse(paused.showOpenAccess)
    }

    @Test
    fun staleRefreshCannotOverwriteANewerSave() {
        assertFalse(phoneReadOperationIsCurrent(operation = 4, current = 5))
        assertTrue(phoneReadOperationIsCurrent(operation = 5, current = 5))
    }
}
