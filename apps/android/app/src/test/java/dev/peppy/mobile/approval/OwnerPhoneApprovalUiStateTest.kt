package dev.peppy.mobile.approval

import dev.peppy.mobile.enrollment.RevealVisibilityState
import org.junit.Assert.*
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test


class OwnerPhoneApprovalUiStateTest {
    @Test fun backgroundPreventsACompletedRevealFromBecomingVisible() {
        val revealVisibility = RevealVisibilityState()
        val operation = revealVisibility.begin()!!
        var revealed: String? = null

        revealVisibility.hide()
        if (revealVisibility.isVisible(operation)) revealed = "vault secret"

        assertFalse(revealed != null)
    }

    @Test fun sharedSecretRevealRequiresAWaitingInvitation() {
        fun view(stage: OwnerPhoneStage) = OwnerPhoneApprovalHost.Result.View(
            OwnerPhoneStatus(stage, 30, null, null, "invitation"),
            "qr",
        )

        assertTrue(canRevealSharedSecret(view(OwnerPhoneStage.WAITING)))
        assertFalse(canRevealSharedSecret(view(OwnerPhoneStage.VERIFY)))
        assertFalse(canRevealSharedSecret(view(OwnerPhoneStage.APPROVED)))
        assertFalse(canRevealSharedSecret(OwnerPhoneApprovalHost.Result.Failed("offline")))
    }

    @Test fun approvalActionRequiresCurrentVerifyAcknowledgementAndAnIdleScreen() {
        val verify = OwnerPhoneStatus(OwnerPhoneStage.VERIFY, 30, "123456", "claim", "invitation")
        val acknowledgement = verify.verificationClaim()

        assertTrue(canApproveOwnerPhoneClaim(verify, acknowledgement, codesMatch = true, busy = false))
        assertFalse(canApproveOwnerPhoneClaim(verify, null, codesMatch = true, busy = false))
        assertFalse(canApproveOwnerPhoneClaim(verify, acknowledgement, codesMatch = false, busy = false))
        assertFalse(canApproveOwnerPhoneClaim(verify, acknowledgement, codesMatch = true, busy = true))
        assertFalse(canApproveOwnerPhoneClaim(verify.copy(stage = OwnerPhoneStage.APPROVED), acknowledgement, codesMatch = true, busy = false))
    }
}
