package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class ContactConsentTest {
    @Test fun alreadyGrantedReadStillRequiresDisclosureBeforeApply() {
        assertEquals(ConsentEffect.NONE, ConsentGatedToggle().agree(true).effect)
        assertEquals(ConsentEffect.APPLY, ConsentGatedToggle().requestEnable().agree(true).effect)
    }

    @Test fun deniedReadOrWriteResultConsumesTheTransientIntent() {
        val denied = ConsentGatedToggle().requestEnable().agree(false).state.accessResult(false)
        assertEquals(ConsentPhase.IDLE, denied.state.phase)
        assertEquals(ConsentEffect.NONE, denied.state.accessResult(true).effect)
    }
}
