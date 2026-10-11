import Testing
@testable import PeppyNative

@Suite struct SelfHostedEnrollmentStateTests {
    @Test func ownerCredentialsAdvanceWithoutRequiringVaultSecret() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        state.setupToken = "operator-code"
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword

        #expect(state.canContinueOwnerSetup)
        #expect(!state.canSubmitOwnerClaim)
    }

    @Test func ownerClaimRequiresSavedGeneratedSecret() {
        var state = validOwnerState()
        state.prepareOwnerSecret { "alpha beta gamma delta epsilon zeta" }

        #expect(!state.canSubmitOwnerClaim)
        state.acknowledgedSecretSave = true
        #expect(state.canSubmitOwnerClaim)
    }

    @Test func ownerClaimGateRejectsBusyOrInactiveSubmission() {
        var state = validOwnerState()
        state.prepareOwnerSecret { "alpha beta gamma delta epsilon zeta" }
        state.acknowledgedSecretSave = true

        #expect(!state.ownerClaimEnabled(isBusy: true, sceneIsActive: true))
        #expect(!state.ownerClaimEnabled(isBusy: false, sceneIsActive: false))
        #expect(state.ownerClaimEnabled(isBusy: false, sceneIsActive: true))
    }

    @Test func preparedSecretRemainsStableAcrossRetry() {
        var state = validOwnerState()
        var generatedValues = ["first stable secret", "silently replaced secret"]

        state.prepareOwnerSecret { generatedValues.removeFirst() }
        state.prepareOwnerSecret { generatedValues.removeFirst() }

        #expect(state.generatedSecret == "first stable secret")
        #expect(generatedValues == ["silently replaced secret"])
    }

    @Test func joiningRequiresOnlyOriginAndManualSharedSecret() {
        var state = SelfHostedEnrollmentState()
        state.joining = true
        state.origin = "https://peppy.example"
        state.sharedSecret = "existing vault secret"

        #expect(state.canSubmitJoin)
    }

    private func validOwnerState() -> SelfHostedEnrollmentState {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        state.setupToken = "operator-code"
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword
        return state
    }
}
