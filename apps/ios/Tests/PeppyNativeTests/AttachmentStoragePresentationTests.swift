import PeppyBindings
import Testing
@testable import PeppyNative

@Suite struct AttachmentStoragePresentationTests {
    private func status(
        cleanup: NativeStorageCleanupState,
        reason: NativeStorageCleanupUnavailableReason? = nil,
        notice: NativeRetentionNoticeState = .eligible,
        used: Int64 = 1,
        limit: Int64 = 2
    ) -> NativeAttachmentStorageStatus {
        NativeAttachmentStorageStatus(
            usedBytes: used, limitBytes: limit, baseBytes: 1, packBytes: 1,
            protectedBytes: 0, removedBytes: 0, cleanupState: cleanup,
            cleanupUnavailableReason: reason,
            notice: NativeRetentionNotice(
                state: notice, policyVersion: attachmentRetentionSupportedNoticePolicyVersion(),
                presentedAtSeconds: nil, acknowledgedAtSeconds: nil, activationAtSeconds: 2
            )
        )
    }

    @Test func fullDisclosureAndAcknowledgementDependOnNoticeStateNotCleanupState() {
        let disabled = AttachmentStoragePresentation(status: status(cleanup: .disabled, notice: .notPresented))
        #expect(disabled.showsThirtyDayDisclosure)
        #expect(disabled.shouldAcknowledgeVisibleCopy)
        #expect(disabled.stateMessageKeys.contains("peppy.attachment_storage_disabled"))

        let eligible = AttachmentStoragePresentation(status: status(cleanup: .available, notice: .eligible))
        #expect(!eligible.showsThirtyDayDisclosure)
        #expect(!eligible.shouldAcknowledgeVisibleCopy)
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

    @Test func globalOffSuppressesRedundantGraceAndDeviceReadinessCopy() {
        let paused = AttachmentStoragePresentation(
            status: status(cleanup: .disabled, notice: .gracePeriod)
        )
        #expect(paused.stateMessageKeys == ["peppy.attachment_storage_disabled"])

        let full = AttachmentStoragePresentation(
            status: status(cleanup: .disabled, notice: .gracePeriod, used: 2, limit: 2)
        )
        #expect(full.stateMessageKeys == [
            "peppy.attachment_storage_disabled", "peppy.attachment_storage_grace_full",
        ])

        let device = AttachmentStoragePresentation(
            status: status(cleanup: .blocked, reason: .deviceNotReady)
        )
        #expect(device.stateMessageKeys == ["peppy.attachment_storage_device_update_required"])
    }

    @Test func fullGraceUsesTheSpecificPausedUploadMessage() {
        let presentation = AttachmentStoragePresentation(
            status: status(cleanup: .blocked, reason: .noticeNotEligible, notice: .gracePeriod, used: 2, limit: 2)
        )
        #expect(presentation.stateMessageKeys == ["peppy.attachment_storage_grace_full"])
    }
}
