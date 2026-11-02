import PeppyBindings
import Testing
@testable import PeppyNative

@Suite struct AttachmentStoragePresentationTests {
    private func status(
        cleanup: NativeStorageCleanupState = .available,
        reason: NativeStorageCleanupUnavailableReason? = nil,
        used: Int64 = 1,
        limit: Int64 = 2
    ) -> NativeAttachmentStorageStatus {
        NativeAttachmentStorageStatus(
            usedBytes: used, limitBytes: limit, baseBytes: 1, packBytes: 1,
            protectedBytes: 0, removedBytes: 0, cleanupState: cleanup,
            cleanupUnavailableReason: reason
        )
    }

    @Test func capacityNoticeUsesNinetyPercentThresholdAndSuppressesInvalidCapacity() {
        #expect(AttachmentStoragePresentation(status: status(used: 89, limit: 100)).capacityNotice == .none)
        #expect(AttachmentStoragePresentation(status: status(used: 90, limit: 100)).capacityNotice == .nearFull)
        #expect(AttachmentStoragePresentation(status: status(used: 99, limit: 100)).capacityNotice == .nearFull)
        #expect(AttachmentStoragePresentation(status: status(used: 100, limit: 100)).capacityNotice == .fullCleanup)
        #expect(AttachmentStoragePresentation(status: status(used: 0, limit: 0)).capacityNotice == .none)
    }

    @Test func typedCleanupReasonsMapToDistinctSharedFacts() {
        let cases: [(NativeStorageCleanupUnavailableReason, String)] = [
            (.deviceNotReady, "peppy.attachment_storage_device_update_required"),
            (.readOnly, "peppy.attachment_storage_read_only"),
            (.billingMutationPending, "peppy.attachment_storage_billing_pending"),
            (.entitlementUnavailable, "peppy.attachment_storage_billing_unavailable"),
            (.providerReconciliationPending, "peppy.attachment_storage_billing_unavailable"),
            (.cleanupDisabled, "peppy.attachment_storage_disabled"),
        ]
        for (reason, key) in cases {
            let presentation = AttachmentStoragePresentation(status: status(cleanup: .blocked, reason: reason))
            #expect(presentation.stateMessageKeys.contains(key))
        }
    }

    @Test func blockedFullCapacityShowsTheCauseWithoutADeletionClaim() {
        let presentation = AttachmentStoragePresentation(
            status: status(cleanup: .blocked, reason: .deviceNotReady, used: 2, limit: 2)
        )
        #expect(presentation.capacityNotice == .cause("peppy.attachment_storage_device_update_required"))
    }

    @Test func blockedNearFullKeepsItsCauseAndOffersTheSharedInformationSheet() {
        let presentation = AttachmentStoragePresentation(
            status: status(cleanup: .blocked, reason: .deviceNotReady, used: 90, limit: 100)
        )

        #expect(presentation.capacityNotice == .nearFullBlocked("peppy.attachment_storage_device_update_required"))
        #expect(presentation.capacityNotice.showsLearnMore)
        #expect(presentation.stateMessageKeys == ["peppy.attachment_storage_device_update_required"])
    }
}
