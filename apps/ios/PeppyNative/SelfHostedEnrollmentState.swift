import Foundation

struct SelfHostedEnrollmentState {
    enum OwnerStep {
        case credentials
        case vaultSecret
    }

    var joining = false
    var ownerStep = OwnerStep.credentials
    var origin = ""
    var setupToken = ""
    var adminPassword = ""
    var confirmation = ""
    var sharedSecret = ""
    private(set) var generatedSecret = ""
    var acknowledgedSecretSave = false
    var revealsGeneratedSecret = false

    var canContinueOwnerSetup: Bool {
        !origin.isEmpty
            && !setupToken.isEmpty
            && adminPassword.utf8.count >= 12
            && adminPassword == confirmation
    }

    var canSubmitOwnerClaim: Bool {
        canContinueOwnerSetup && !generatedSecret.isEmpty && acknowledgedSecretSave
    }

    var canSubmitJoin: Bool {
        !origin.isEmpty && !sharedSecret.isEmpty
    }

    func ownerClaimEnabled(isBusy: Bool, sceneIsActive: Bool) -> Bool {
        canSubmitOwnerClaim && !isBusy && sceneIsActive
    }

    mutating func prepareOwnerSecret(using generate: () -> String) {
        if generatedSecret.isEmpty {
            generatedSecret = generate()
        }
        ownerStep = .vaultSecret
    }

    mutating func switchMode() {
        joining.toggle()
        clearSecrets()
    }

    mutating func clearSecrets() {
        ownerStep = .credentials
        setupToken = ""
        adminPassword = ""
        confirmation = ""
        sharedSecret = ""
        generatedSecret = ""
        acknowledgedSecretSave = false
        revealsGeneratedSecret = false
    }
}
