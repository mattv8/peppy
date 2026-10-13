import Testing
@testable import PeppyNative

@Suite struct SelfHostedEnrollmentStateTests {
    @Test func ownerCredentialsAdvanceWithoutRequiringVaultSecret() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        let epoch = state.beginDiscovery()
        state.applyDiscovery(canonicalOrigin: state.origin, setupCodeRequired: true, requestedOrigin: state.origin, epoch: epoch)
        state.setupToken = "operator-code"
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword

        #expect(state.canContinueOwnerSetup)
        #expect(!state.canSubmitOwnerClaim)
    }

    @Test func openDiscoveryAllowsAnEmptySetupCode() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        let epoch = state.beginDiscovery()
        state.applyDiscovery(canonicalOrigin: "https://peppy.example", setupCodeRequired: false, requestedOrigin: state.origin, epoch: epoch)
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword

        #expect(state.canContinueOwnerSetup)
    }

    @Test func changingTheOriginInvalidatesOpenDiscovery() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        let epoch = state.beginDiscovery()
        state.applyDiscovery(canonicalOrigin: "https://peppy.example", setupCodeRequired: false, requestedOrigin: state.origin, epoch: epoch)
        state.origin = "https://other.example"
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword

        #expect(!state.canContinueOwnerSetup)
        #expect(state.setupCodeRequired)
        #expect(state.setupToken.isEmpty)
    }

    @Test func modeSwitchRejectsAStaleDiscoveryForTheSameOrigin() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        let epoch = state.beginDiscovery()
        state.switchMode()
        state.applyDiscovery(
            canonicalOrigin: "https://peppy.example",
            setupCodeRequired: false,
            requestedOrigin: "https://peppy.example",
            epoch: epoch
        )

        #expect(state.discoveredOrigin == nil)
        #expect(state.setupCodeRequired)
    }

    @Test func ownerClaimUsesGeneratedSecretWithoutAcknowledgement() {
        var state = validOwnerState()
        state.prepareOwnerSecret { "alpha beta gamma delta epsilon zeta" }

        #expect(state.canSubmitOwnerClaim)
    }

    @Test func ownerClaimGateRejectsBusyOrInactiveSubmission() {
        var state = validOwnerState()
        state.prepareOwnerSecret { "alpha beta gamma delta epsilon zeta" }

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

    @Test func vaultCreationUsesStableGeneratedSecretWithoutAcknowledgement() {
        var state = SelfHostedEnrollmentState()
        state.vaultUsername = "family"
        state.vaultPassword = "long-owner-password"
        state.vaultPasswordConfirmation = state.vaultPassword
        state.prepareVaultSecret { "first stable secret" }
        state.prepareVaultSecret { "replacement secret" }

        #expect(state.canSubmitVaultCreation)
        #expect(state.generatedSecret == "first stable secret")
    }

    @Test func inviteResetClearsPreparedSecretButLeavesClaimedOriginAvailable() {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        state.claimedChooserActive = true
        state.beginInviteRequest(link: "https://peppy.example/#invite=token")
        state.prepareVaultSecret { "saved secret" }

        state.clearInvite()

        #expect(state.inviteStage == .none)
        #expect(state.generatedSecret.isEmpty)
        #expect(state.origin == "https://peppy.example")
    }

    @Test func preparedVaultWithoutStoredSecretRequiresEnteredSecret() {
        var state = vaultCreationState()
        state.applyPreparedOwnerMaterial(storedSecret: false)

        #expect(state.requiresEnteredSecret)
        #expect(!state.requiresSavedSecret)
        #expect(!state.canSubmitVaultCreation, "an entry is required before the request")
        state.savedSecret = "entered secret"
        #expect(state.canSubmitVaultCreation)
    }

    @Test func preparedVaultWithStoredSecretNeedsNoEntry() {
        var state = vaultCreationState()
        state.applyPreparedOwnerMaterial(storedSecret: true)

        #expect(state.requiresSavedSecret)
        #expect(!state.requiresEnteredSecret)
        #expect(state.canSubmitVaultCreation)
    }

    @Test func savedSecretHintAppearsOnlyWhenTheSecretIsAlreadyStored() {
        var state = vaultCreationState()
        state.prepareVaultSecret { "generated secret" }
        #expect(!state.savedSecretHintVisible, "a generated but unwritten secret is not yet stored")

        state.applyPreparedOwnerMaterial(storedSecret: true)
        #expect(state.savedSecretHintVisible)
    }

    @Test func clearingTheInviteDropsAnEnteredSecret() {
        var state = vaultCreationState()
        state.applyPreparedOwnerMaterial(storedSecret: false)
        state.savedSecret = "entered secret"

        state.clearInvite()

        #expect(!state.requiresEnteredSecret)
        #expect(state.savedSecret.isEmpty)
    }

    private func vaultCreationState() -> SelfHostedEnrollmentState {
        var state = SelfHostedEnrollmentState()
        state.vaultUsername = "family"
        state.vaultPassword = "long-owner-password"
        state.vaultPasswordConfirmation = state.vaultPassword
        return state
    }

    private func validOwnerState() -> SelfHostedEnrollmentState {
        var state = SelfHostedEnrollmentState()
        state.origin = "https://peppy.example"
        let epoch = state.beginDiscovery()
        state.applyDiscovery(canonicalOrigin: state.origin, setupCodeRequired: true, requestedOrigin: state.origin, epoch: epoch)
        state.setupToken = "operator-code"
        state.adminPassword = "long-admin-password"
        state.confirmation = state.adminPassword
        return state
    }
}
