package dev.peppy.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollmentUiStateTest {
    private val strongAdminPassword = "a sufficiently strong password"
    private val secret = "amber birch cedar dune ember frost"

    @Test fun newOwnerClaimNeedsANonBlankGeneratedSecret() {
        assertFalse(canClaimOwner(false, false, "setup-code", true, strongAdminPassword, strongAdminPassword, "", false))
        assertTrue(canClaimOwner(false, false, "setup-code", true, strongAdminPassword, strongAdminPassword, secret, false))
        assertFalse(canClaimOwner(false, false, "setup-code", true, strongAdminPassword, strongAdminPassword, secret, true))
        assertFalse(canClaimOwner(false, false, "setup-code", true, "short", "short", secret, false))
    }

    @Test fun storedResumeSecretNeedsNoEnteredSecret() {
        assertTrue(canClaimOwner(true, true, "setup-code", true, strongAdminPassword, strongAdminPassword, "", false))
    }

    @Test fun legacyResumeWithoutStoredSecretRequiresAnEnteredSecret() {
        assertFalse(canClaimOwner(true, false, "setup-code", true, strongAdminPassword, strongAdminPassword, "", false))
        assertTrue(canClaimOwner(true, false, "setup-code", true, strongAdminPassword, strongAdminPassword, secret, false))
    }

    @Test fun protectedResumeStillRequiresSetupCodeAndAdminPassword() {
        assertFalse(canClaimOwner(true, true, "", true, strongAdminPassword, strongAdminPassword, "", false))
        assertFalse(canClaimOwner(true, true, "setup-code", true, "short", "short", "", false))
        assertFalse(canClaimOwner(true, true, "setup-code", true, strongAdminPassword, strongAdminPassword, "", true))
    }

    @Test fun openOwnerClaimAndResumePermitAnEmptySetupCode() {
        assertTrue(canClaimOwner(false, false, "", false, strongAdminPassword, strongAdminPassword, secret, false))
        assertTrue(canClaimOwner(true, true, "", false, strongAdminPassword, strongAdminPassword, "", false))
    }

    @Test fun ownerClaimRequiresMatchingConfirmedAdminPassword() {
        assertFalse(canClaimOwner(false, false, "setup-code", true, strongAdminPassword, "different password", secret, false))
        assertTrue(canClaimOwner(false, false, "setup-code", true, strongAdminPassword, strongAdminPassword, secret, false))
        assertFalse(canClaimOwner(false, false, "setup-code", true, "", "", secret, false))
    }

    @Test fun adminPasswordUsesUtf8ByteBoundaries() {
        val twelveBytePassword = "é".repeat(6)
        val maximumPassword = "a".repeat(1024)
        val oversizedPassword = "a".repeat(1025)

        assertTrue(hasValidAdminPasswordLength(twelveBytePassword))
        assertTrue(hasValidAdminPasswordLength(maximumPassword))
        assertFalse(hasValidAdminPasswordLength("é".repeat(5)))
        assertFalse(hasValidAdminPasswordLength(oversizedPassword))
    }

    @Test fun secretFieldIsShownOnlyWhenResumeHasNoStoredSecret() {
        assertTrue(ownerSecretFieldVisible(ownerResume = true, ownerSecretStored = false))
        assertFalse(ownerSecretFieldVisible(ownerResume = true, ownerSecretStored = true))
        assertFalse(ownerSecretFieldVisible(ownerResume = false, ownerSecretStored = false))
    }

    @Test fun vaultRecoveryFieldIsShownOnlyWhenPendingSecretIsMissing() {
        assertTrue(recoverySecretFieldVisible(EnrollmentHost.ResumeSecret.PROMPT))
        assertFalse(recoverySecretFieldVisible(EnrollmentHost.ResumeSecret.STORED))
        assertFalse(recoverySecretFieldVisible(EnrollmentHost.ResumeSecret.NONE))
        assertFalse(recoverySecretFieldVisible(EnrollmentHost.ResumeSecret.UNAVAILABLE))
    }

    @Test fun vaultCreateNeedsAnEnteredOrStoredSecretAndNeverProceedsWhenStorageIsUnavailable() {
        assertTrue(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.NONE, false))
        assertTrue(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, "", EnrollmentHost.ResumeSecret.STORED, false))
        assertFalse(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, "", EnrollmentHost.ResumeSecret.PROMPT, false))
        assertTrue(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.PROMPT, false))
        assertFalse(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.UNAVAILABLE, false))
        assertFalse(canCreateVault("", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.NONE, false))
        assertFalse(canCreateVault("taylor", "short", "short", secret, EnrollmentHost.ResumeSecret.NONE, false))
        assertFalse(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.NONE, true))
    }

    @Test fun vaultCreateRequiresMatchingConfirmedAdminPassword() {
        assertFalse(canCreateVault("taylor", strongAdminPassword, "different password", secret, EnrollmentHost.ResumeSecret.NONE, false))
        assertTrue(canCreateVault("taylor", strongAdminPassword, strongAdminPassword, secret, EnrollmentHost.ResumeSecret.NONE, false))
    }

    @Test fun ownerSecretErrorCodesMapToGeneratedNativeCopy() {
        assertEquals(R.string.peppy_owner_phone_error_wrong_passphrase, ownerSecretErrorMessage("wrong-passphrase"))
        assertEquals(R.string.peppy_owner_phone_error_secret_mismatch, ownerSecretErrorMessage("vault-secret-mismatch"))
        assertNull(ownerSecretErrorMessage("untrusted_server_message"))
    }

    @Test fun enrollmentErrorCodesMapToGeneratedNativeCopy() {
        assertEquals(R.string.peppy_enrollment_error_vault_owner_required, enrollmentErrorMessage("vault_owner_required"))
        assertEquals(R.string.peppy_enrollment_error_invite_invalid, enrollmentErrorMessage("invite_invalid"))
        assertEquals(R.string.peppy_enrollment_error_create_vault_limited, enrollmentErrorMessage("create_vault_limited"))
        assertNull(enrollmentErrorMessage("untrusted_server_message"))
    }

    @Test fun automaticPhoneConfirmationRequiresApprovalTransferAndDeliversItsResultOnce() {
        val attempt = PhoneEnrollmentAttempt()
        var successDeliveries = 0
        var failures = 0

        assertFalse(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.PENDING, false))
        assertFalse(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.APPROVED, true))
        assertTrue(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.APPROVED, false))
        assertFalse(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.APPROVED, false))
        assertEquals(
            PhoneEnrollmentAttempt.Completion.SUCCEEDED,
            attempt.deliverCompletion(ImportResult.IMPORTED, current = true, onSuccess = { successDeliveries += 1 }, onFailure = { failures += 1 }),
        )
        assertEquals(1, successDeliveries)
        assertEquals(0, failures)
        assertEquals(
            PhoneEnrollmentAttempt.Completion.IGNORED,
            attempt.deliverCompletion(ImportResult.UPDATED, current = true, onSuccess = { successDeliveries += 1 }, onFailure = { failures += 1 }),
        )
        assertEquals(1, successDeliveries)
    }

    @Test fun automaticPhoneConfirmationFailureIsDeliveredForTerminalRecovery() {
        val attempt = PhoneEnrollmentAttempt()
        var failures = 0

        assertTrue(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.APPROVED, false))
        assertEquals(
            PhoneEnrollmentAttempt.Completion.FAILED,
            attempt.deliverCompletion(ImportResult.NETWORK_ERROR, current = true, onSuccess = {}, onFailure = { failures += 1 }),
        )
        assertEquals(1, failures)
    }

    @Test fun canceledAutomaticPhoneConfirmationCannotDeliverAnEnrollment() {
        val attempt = PhoneEnrollmentAttempt()
        var successDeliveries = 0
        var failures = 0
        assertTrue(attempt.startAutomaticConfirmation(EnrollmentHost.PhoneState.APPROVED, false))

        attempt.cancel()

        assertEquals(
            PhoneEnrollmentAttempt.Completion.IGNORED,
            attempt.deliverCompletion(ImportResult.IMPORTED, current = true, onSuccess = { successDeliveries += 1 }, onFailure = { failures += 1 }),
        )
        assertEquals(0, successDeliveries)
        assertEquals(0, failures)
    }
}
