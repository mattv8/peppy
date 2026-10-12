import Foundation

struct SelfHostedEnrollmentState {
    enum OwnerStep {
        case credentials
        case vaultSecret
    }

    enum InviteStage {
        case none, request, waiting, approved, denied, expired
    }

    var joining = false
    var signingIn = false
    var signInUsername = ""
    var signInPassword = ""
    var ownerStep = OwnerStep.credentials
    var origin = "" {
        didSet {
            guard origin != oldValue else { return }
            invalidateDiscovery()
        }
    }
    private(set) var discoveredOrigin: String?
    var setupCodeRequired = true
    private var discoveryEpoch = 0
    var setupToken = ""
    var ownerUsername = "admin"
    var adminPassword = ""
    var confirmation = ""
    var sharedSecret = ""
    private(set) var generatedSecret = ""
    var acknowledgedSecretSave = false
    var revealsGeneratedSecret = false
    var claimedChooserActive = false
    var inviteLink = ""
    var displayName = ""
    var vaultUsername = ""
    var vaultPassword = ""
    var vaultPasswordConfirmation = ""
    var vaultSecretPrepared = false
    var requiresSavedSecret = false
    var savedSecret = ""
    var inviteStage = InviteStage.none
    var vaultOwner = ""
    private(set) var inviteEpoch = 0

    var canContinueOwnerSetup: Bool {
        discoveredOrigin == origin
            && (!setupCodeRequired || !setupToken.isEmpty)
            && adminPassword.utf8.count >= 12
            && adminPassword == confirmation
    }

    var canSubmitOwnerClaim: Bool {
        canContinueOwnerSetup && !generatedSecret.isEmpty && acknowledgedSecretSave
    }

    var canSubmitJoin: Bool {
        !origin.isEmpty && !sharedSecret.isEmpty
    }

    var canSubmitVaultCreation: Bool {
        !vaultUsername.isEmpty && vaultPassword.utf8.count >= 12
            && vaultPassword == vaultPasswordConfirmation
            && (requiresSavedSecret ? !savedSecret.isEmpty : !generatedSecret.isEmpty) && acknowledgedSecretSave
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

    mutating func beginDiscovery() -> Int {
        discoveryEpoch &+= 1
        return discoveryEpoch
    }

    mutating func applyDiscovery(
        canonicalOrigin: String,
        setupCodeRequired: Bool,
        requestedOrigin: String,
        epoch: Int
    ) {
        guard !joining, origin == requestedOrigin, discoveryEpoch == epoch else { return }
        origin = canonicalOrigin
        discoveredOrigin = canonicalOrigin
        self.setupCodeRequired = setupCodeRequired
    }

    mutating func invalidateDiscovery() {
        discoveryEpoch &+= 1
        discoveredOrigin = nil
        setupCodeRequired = true
        setupToken = ""
    }

    mutating func clearSecrets() {
        ownerStep = .credentials
        setupToken = ""
        adminPassword = ""
        confirmation = ""
        sharedSecret = ""
        signInPassword = ""
        generatedSecret = ""
        acknowledgedSecretSave = false
        revealsGeneratedSecret = false
        invalidateDiscovery()
    }

    mutating func beginInviteRequest(link: String = "") {
        inviteEpoch &+= 1
        inviteLink = link
        inviteStage = .request
        claimedChooserActive = false
    }

    mutating func prepareVaultSecret(using generate: () -> String) {
        if generatedSecret.isEmpty { generatedSecret = generate() }
        vaultSecretPrepared = true
    }

    mutating func clearInvite() {
        inviteEpoch &+= 1
        inviteLink = ""
        displayName = ""
        vaultUsername = ""
        vaultPassword = ""
        vaultPasswordConfirmation = ""
        generatedSecret = ""
        acknowledgedSecretSave = false
        revealsGeneratedSecret = false
        vaultSecretPrepared = false
        requiresSavedSecret = false
        savedSecret = ""
        inviteStage = .none
    }

    func isCurrentInviteEpoch(_ epoch: Int) -> Bool { inviteEpoch == epoch }
}
