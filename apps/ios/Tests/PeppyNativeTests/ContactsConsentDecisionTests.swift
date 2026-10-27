import Testing
@testable import PeppyNative

@Suite struct ContactsConsentDecisionTests {
    @Test func disclosureDoesNotRequestOrApplyUntilAgreement() {
        var decision = ContactsConsentDecision()
        decision.requestEnable()
        #expect(decision.phase == .disclosure)
        #expect(decision.agree(access: .full) == .apply)
        #expect(decision.phase == .applying)
    }

    @Test func deniedAndCancelledRequestsNeverApplyLater() {
        var decision = ContactsConsentDecision()
        decision.requestEnable()
        #expect(decision.agree(access: .notDetermined) == .requestAccess)
        decision.cancel()
        #expect(decision.accessResult(granted: true) == .none)
        #expect(decision.phase == .idle)

        decision.requestEnable()
        #expect(decision.agree(access: .notDetermined) == .requestAccess)
        #expect(decision.accessResult(granted: false) == .none)
        #expect(decision.phase == .idle)
    }

    @Test func existingAccessStillRequiresDisclosureAndDeniedOpensSettings() {
        var decision = ContactsConsentDecision()
        #expect(decision.agree(access: .full) == .none)
        decision.requestEnable()
        #expect(decision.agree(access: .denied) == .openSettings)
        #expect(decision.phase == .awaitingSettings)
    }

    @Test func settingsReturnAndDoubleAgreementConsumeOnlyCurrentDecision() {
        var decision = ContactsConsentDecision()
        decision.requestEnable()
        #expect(decision.agree(access: .denied) == .openSettings)
        #expect(decision.phase == .awaitingSettings)
        #expect(decision.settingsAccessResult(granted: true) == .apply)
        #expect(decision.phase == .applying)

        decision.finish()
        decision.requestEnable()
        #expect(decision.agree(access: .full) == .apply)
        #expect(decision.agree(access: .full) == .none)
        decision.cancel()
        #expect(decision.accessResult(granted: true) == .none)
    }

    @Test func staleSettingsGrantAndCancellationCannotApply() {
        var decision = ContactsConsentDecision()
        decision.requestEnable()
        #expect(decision.agree(access: .denied) == .openSettings)
        decision.cancel()
        #expect(decision.settingsAccessResult(granted: true) == .none)
        #expect(decision.phase == .idle)
    }

    @Test func settingsDenialConsumesTheOnlyPendingIntent() {
        var decision = ContactsConsentDecision()
        decision.requestEnable()
        #expect(decision.agree(access: .denied) == .openSettings)
        #expect(decision.settingsAccessResult(granted: false) == .none)
        #expect(decision.settingsAccessResult(granted: true) == .none)
        #expect(decision.phase == .idle)
    }
}
