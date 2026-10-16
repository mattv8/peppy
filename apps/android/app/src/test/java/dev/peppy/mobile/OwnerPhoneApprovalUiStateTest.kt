package dev.peppy.mobile

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
}
