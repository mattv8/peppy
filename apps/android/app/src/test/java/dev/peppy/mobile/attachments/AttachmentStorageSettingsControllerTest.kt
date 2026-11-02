package dev.peppy.mobile.attachments

import org.junit.Assert.*
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

    @Test fun refreshPreservesLastStatusWhileUnavailable() {
        val controller = AttachmentStorageSettingsController()
        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.Available(sampleStatus()))

        controller.begin("session")
        controller.apply("session", AttachmentStorageLoad.TemporarilyUnavailable)

        assertTrue(controller.state is AttachmentStorageUiState.Unavailable)
        assertEquals(42L, (controller.state as AttachmentStorageUiState.Unavailable).lastKnown?.usedBytes)
    }

    @Test fun capacityNoticeUsesTheNinetyPercentThresholdAndNeverUsesInvalidCapacity() {
        val cases = listOf(
            89L to AttachmentStorageCapacityNotice.None,
            90L to AttachmentStorageCapacityNotice.NearFull,
            99L to AttachmentStorageCapacityNotice.NearFull,
            100L to AttachmentStorageCapacityNotice.FullCleanup,
        )

        for ((used, expected) in cases) {
            assertEquals(expected, attachmentStorageCapacityNotice(sampleStatus(usedBytes = used)))
        }
        assertEquals(AttachmentStorageCapacityNotice.None, attachmentStorageCapacityNotice(sampleStatus(limitBytes = 0)))
        assertEquals(AttachmentStorageCapacityNotice.None, attachmentStorageCapacityNotice(sampleStatus(limitBytes = -1)))
    }

    @Test fun blockedCapacityShowsNearFullWithItsCauseAndKeepsItsCauseAtFull() {
        val blocked = sampleStatus(cleanup = AttachmentStorageCleanup.BLOCKED, reason = AttachmentStorageUnavailableReason.DEVICE_NOT_READY)

        assertEquals(
            AttachmentStorageCapacityNotice.NearFullBlocked("peppy.attachment_storage_device_update_required"),
            attachmentStorageCapacityNotice(blocked.copy(usedBytes = 90)),
        )
        assertEquals(
            AttachmentStorageCapacityNotice.Cause("peppy.attachment_storage_device_update_required"),
            attachmentStorageCapacityNotice(blocked.copy(usedBytes = 100)),
        )
    }

    @Test fun unknownCleanupReasonNeverClaimsAutomaticRemoval() {
        val unknown = sampleStatus(cleanup = AttachmentStorageCleanup.BLOCKED, reason = AttachmentStorageUnavailableReason.UNKNOWN)

        assertEquals(
            AttachmentStorageCapacityNotice.Cause("peppy.attachment_storage_unavailable"),
            attachmentStorageCapacityNotice(unknown.copy(usedBytes = 100)),
        )
    }

    private fun sampleStatus(
        usedBytes: Long = 42,
        limitBytes: Long = 100,
        cleanup: AttachmentStorageCleanup = AttachmentStorageCleanup.AVAILABLE,
        reason: AttachmentStorageUnavailableReason? = null,
    ) = AttachmentStorageSnapshot(
        usedBytes = usedBytes,
        limitBytes = limitBytes,
        baseBytes = 50,
        cleanup = cleanup,
        unavailableReason = reason,
    )
}
