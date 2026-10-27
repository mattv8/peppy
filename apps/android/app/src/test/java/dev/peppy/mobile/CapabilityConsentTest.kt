package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class CapabilityConsentTest {
    @Test fun requestEnableOpensDisclosureOnlyFromIdle() {
        assertEquals(ConsentPhase.DISCLOSURE, ConsentGatedToggle().requestEnable().phase)
        assertEquals(ConsentPhase.DISCLOSURE, ConsentGatedToggle(ConsentPhase.DISCLOSURE).requestEnable().phase)
        assertEquals(ConsentPhase.AWAITING_ACCESS, ConsentGatedToggle(ConsentPhase.AWAITING_ACCESS).requestEnable().phase)
        assertEquals(ConsentPhase.APPLYING, ConsentGatedToggle(ConsentPhase.APPLYING).requestEnable().phase)
    }

    @Test fun agreeWithExistingAccessAppliesWithoutRequestingAccess() {
        val transition = ConsentGatedToggle().requestEnable().agree(hasAccess = true)

        assertEquals(ConsentPhase.APPLYING, transition.state.phase)
        assertEquals(ConsentEffect.APPLY, transition.effect)
    }

    @Test fun agreeWithoutAccessRequestsAccessExactlyOnce() {
        val awaiting = ConsentGatedToggle().requestEnable().agree(hasAccess = false)

        assertEquals(ConsentPhase.AWAITING_ACCESS, awaiting.state.phase)
        assertEquals(ConsentEffect.REQUEST_ACCESS, awaiting.effect)

        val duplicate = awaiting.state.agree(hasAccess = false)
        assertEquals(ConsentPhase.AWAITING_ACCESS, duplicate.state.phase)
        assertEquals(ConsentEffect.NONE, duplicate.effect)
    }

    @Test fun duplicateAgreeAfterApplyIsIgnored() {
        val applying = ConsentGatedToggle().requestEnable().agree(hasAccess = true).state

        val duplicate = applying.agree(hasAccess = true)

        assertEquals(ConsentPhase.APPLYING, duplicate.state.phase)
        assertEquals(ConsentEffect.NONE, duplicate.effect)
    }

    @Test fun agreeOutsideDisclosureDoesNothing() {
        val idle = ConsentGatedToggle().agree(hasAccess = true)
        assertEquals(ConsentPhase.IDLE, idle.state.phase)
        assertEquals(ConsentEffect.NONE, idle.effect)

        val applying = ConsentGatedToggle(ConsentPhase.APPLYING).agree(hasAccess = false)
        assertEquals(ConsentPhase.APPLYING, applying.state.phase)
        assertEquals(ConsentEffect.NONE, applying.effect)
    }

    @Test fun grantedAccessResultAppliesPolicy() {
        val awaiting = ConsentGatedToggle().requestEnable().agree(hasAccess = false).state

        val transition = awaiting.accessResult(granted = true)

        assertEquals(ConsentPhase.APPLYING, transition.state.phase)
        assertEquals(ConsentEffect.APPLY, transition.effect)
    }

    @Test fun deniedAccessResultConsumesIntentAndStaysOff() {
        val awaiting = ConsentGatedToggle().requestEnable().agree(hasAccess = false).state

        val denied = awaiting.accessResult(granted = false)

        assertEquals(ConsentPhase.IDLE, denied.state.phase)
        assertEquals(ConsentEffect.NONE, denied.effect)
        // A later grant without a new explicit intent must not enable anything.
        val lateGrant = denied.state.accessResult(granted = true)
        assertEquals(ConsentPhase.IDLE, lateGrant.state.phase)
        assertEquals(ConsentEffect.NONE, lateGrant.effect)
    }

    @Test fun cancelledPendingRequestIgnoresLateGrant() {
        val awaiting = ConsentGatedToggle().requestEnable().agree(hasAccess = false).state

        val cancelled = awaiting.cancel()
        assertEquals(ConsentPhase.IDLE, cancelled.phase)

        val lateGrant = cancelled.accessResult(granted = true)
        assertEquals(ConsentPhase.IDLE, lateGrant.state.phase)
        assertEquals(ConsentEffect.NONE, lateGrant.effect)
    }

    @Test fun grantAloneInIdleNeverEnables() {
        val transition = ConsentGatedToggle().accessResult(granted = true)

        assertEquals(ConsentPhase.IDLE, transition.state.phase)
        assertEquals(ConsentEffect.NONE, transition.effect)
    }

    @Test fun grantAloneInDisclosureNeverEnables() {
        val transition = ConsentGatedToggle().requestEnable().accessResult(granted = true)

        assertEquals(ConsentPhase.DISCLOSURE, transition.state.phase)
        assertEquals(ConsentEffect.NONE, transition.effect)
    }

    @Test fun reenablingAfterOffRequiresConsentAgainEvenWithAccess() {
        val applied = ConsentGatedToggle().requestEnable().agree(hasAccess = true).state
        val off = applied.cancel()

        assertEquals(ConsentPhase.IDLE, off.phase)
        val directApply = off.agree(hasAccess = true)
        assertEquals(ConsentEffect.NONE, directApply.effect)
        assertEquals(ConsentPhase.IDLE, directApply.state.phase)

        val reopened = off.requestEnable()
        assertEquals(ConsentPhase.DISCLOSURE, reopened.phase)
    }

    @Test fun applyThenResetReturnsToIdle() {
        val applying = ConsentGatedToggle().requestEnable().agree(hasAccess = true).state

        assertEquals(ConsentPhase.IDLE, applying.cancel().phase)
    }

    @Test fun cancelResetsEveryPhaseToIdle() {
        listOf(
            ConsentGatedToggle(),
            ConsentGatedToggle(ConsentPhase.DISCLOSURE),
            ConsentGatedToggle(ConsentPhase.AWAITING_ACCESS),
            ConsentGatedToggle(ConsentPhase.APPLYING),
        ).forEach { state ->
            assertEquals(ConsentGatedToggle(), state.cancel())
        }
    }

    @Test fun transitionsDoNotMutateOriginalState() {
        val disclosure = ConsentGatedToggle().requestEnable()

        disclosure.agree(hasAccess = false)
        disclosure.accessResult(granted = true)

        assertEquals(ConsentPhase.DISCLOSURE, disclosure.phase)
    }
}
