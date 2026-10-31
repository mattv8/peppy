package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AttachmentStorageSettingsControllerTest {
    @Test fun staleStatusDoesNotReplaceTheNewerSession() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("first")
        controller.begin("second")

        assertFalse(controller.apply("first", AttachmentStorageLoad.Available(sampleStatus())))
        assertTrue(controller.apply("second", AttachmentStorageLoad.Unsupported))
        assertEquals(AttachmentStorageUiState.Unsupported, controller.state)
    }

    @Test fun unknownPolicyVersionNeverBecomesAcknowledgable() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus(policyVersion = "v2")))

        assertFalse(controller.claimDisclosureRendered("session"))
    }

    @Test fun acknowledgementWaitsUntilTheCompleteDisclosureRenders() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))

        assertFalse(controller.acknowledgeResult("session", true))
        assertTrue(controller.claimDisclosureRendered("session"))
        assertTrue(controller.acknowledgeResult("session", true))
        assertFalse(controller.claimDisclosureRendered("session"))
    }

    @Test fun refreshPreservesLastStatusWhileUnavailable() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))

        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.TemporarilyUnavailable)

        assertTrue(controller.state is AttachmentStorageUiState.Unavailable)
        assertEquals(42L, (controller.state as AttachmentStorageUiState.Unavailable).lastKnown?.usedBytes)
    }

    @Test fun serverActivationTimestampIsPreservedWithoutClientDeadlineMath() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))

        assertEquals(1_700_000_000L, (controller.state as AttachmentStorageUiState.Available).status.notice.activationAtSeconds)
    }

    @Test fun failedAcknowledgementCanOnlyBeRetriedForTheSameVisibleScope() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))
        assertTrue(controller.claimDisclosureRendered("session"))
        assertFalse(controller.acknowledgeResult("session", false))
        assertFalse(controller.retryFailedAcknowledgement("other"))
        assertTrue(controller.retryFailedAcknowledgement("session"))
    }

    @Test fun explicitFailedRetryClaimsOnceAndDoesNotRepeatAfterSuccess() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))
        assertTrue(controller.claimDisclosureRendered("session"))
        assertFalse(controller.acknowledgeResult("session", false))
        assertTrue(controller.retryFailedAcknowledgement("session"))
        assertTrue(controller.acknowledgeResult("session", true))
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))
        assertFalse(controller.claimDisclosureRendered("session"))
    }

    private fun sampleStatus(policyVersion: String = "v1") = AttachmentStorageSnapshot(
        usedBytes = 42,
        limitBytes = 100,
        baseBytes = 50,
        cleanup = AttachmentStorageCleanup.DISABLED,
        unavailableReason = AttachmentStorageUnavailableReason.CLEANUP_DISABLED,
        notice = AttachmentStorageNotice(policyVersion, AttachmentStorageNoticeState.NOT_PRESENTED, 1_700_000_000L),
        noticeCopySupported = policyVersion == "v1",
    )
}
