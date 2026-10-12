package dev.peppy.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollmentUiStateTest {
    private val strongAdminPassword = "a sufficiently strong password"
    private val secret = "amber birch cedar dune ember frost"

    @Test fun claimEligibilityRequiresASavedNonBlankSecretForANewOwner() {
        assertFalse(canClaimOwner(false, "setup-code", true, strongAdminPassword, "", true, false))
        assertFalse(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, false, false))
        assertTrue(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, true, false))
    }

    @Test fun claimEligibilityKeepsTheSameSecretEligibleAcrossRepeatedAcknowledgementToggles() {
        assertTrue(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, true, false))
        assertFalse(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, false, false))
        assertTrue(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, true, false))
    }

    @Test fun claimEligibilityRejectsBusyAndWeakAdminStates() {
        assertFalse(canClaimOwner(false, "setup-code", true, strongAdminPassword, secret, true, true))
        assertFalse(canClaimOwner(false, "setup-code", true, "short", secret, true, false))
    }

    @Test fun openOwnerClaimPermitsAnEmptySetupCode() {
        assertTrue(canClaimOwner(false, "", false, strongAdminPassword, secret, true, false))
    }

    @Test fun protectedOwnerResumeStillRequiresSetupCode() {
        assertFalse(canClaimOwner(true, "", true, strongAdminPassword, secret, false, false))
        assertTrue(canClaimOwner(true, "setup-code", true, strongAdminPassword, secret, false, false))
        assertFalse(canClaimOwner(true, "setup-code", true, strongAdminPassword, "", false, false))
        assertFalse(canClaimOwner(true, "setup-code", true, "short", secret, false, false))
    }

    @Test fun openOwnerResumePermitsAnEmptySetupCode() {
        assertTrue(canClaimOwner(true, "", false, strongAdminPassword, secret, false, false))
    }
}
