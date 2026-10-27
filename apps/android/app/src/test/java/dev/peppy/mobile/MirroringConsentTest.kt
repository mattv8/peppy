package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class MirroringConsentTest {
    @Test fun disclosureDeclineAndLateAccessResultDoNotApply() {
        val dismissed = ConsentGatedToggle().requestEnable().cancel()
        assertEquals(ConsentEffect.NONE, dismissed.accessResult(true).effect)
    }

    @Test fun agreeRequestsAccessOnceThenAppliesOnlyFirstGrantedResult() {
        val awaiting = ConsentGatedToggle().requestEnable().agree(false)
        assertEquals(ConsentEffect.REQUEST_ACCESS, awaiting.effect)
        val applied = awaiting.state.accessResult(true)
        assertEquals(ConsentEffect.APPLY, applied.effect)
        assertEquals(ConsentEffect.NONE, applied.state.accessResult(true).effect)
    }
}
