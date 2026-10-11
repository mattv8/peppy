package dev.peppy.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollmentUiStateTest {
    private val strongAdminPassword = "a sufficiently strong password"
    private val secret = "amber birch cedar dune ember frost"

    @Test fun claimEligibilityRequiresASavedNonBlankSecretForANewOwner() {
        assertFalse(canClaimOwner(false, "setup-code", strongAdminPassword, "", true, false))
        assertFalse(canClaimOwner(false, "setup-code", strongAdminPassword, secret, false, false))
        assertTrue(canClaimOwner(false, "setup-code", strongAdminPassword, secret, true, false))
    }

    @Test fun claimEligibilityKeepsTheSameSecretEligibleAcrossRepeatedAcknowledgementToggles() {
        assertTrue(canClaimOwner(false, "setup-code", strongAdminPassword, secret, true, false))
        assertFalse(canClaimOwner(false, "setup-code", strongAdminPassword, secret, false, false))
        assertTrue(canClaimOwner(false, "setup-code", strongAdminPassword, secret, true, false))
    }

    @Test fun claimEligibilityRejectsBusyAndWeakAdminStates() {
        assertFalse(canClaimOwner(false, "setup-code", strongAdminPassword, secret, true, true))
        assertFalse(canClaimOwner(false, "setup-code", "short", secret, true, false))
    }

    @Test fun resumedOwnerClaimDoesNotNeedSetupCodeButStillNeedsSecretAndAdminPassword() {
        assertTrue(canClaimOwner(true, "", strongAdminPassword, secret, false, false))
        assertFalse(canClaimOwner(true, "", strongAdminPassword, "", false, false))
        assertFalse(canClaimOwner(true, "", "short", secret, false, false))
    }
}
