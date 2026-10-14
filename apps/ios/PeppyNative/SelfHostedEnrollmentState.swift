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
    /// True only after the native host confirms a protected pending secret for this origin.
    var hasPendingSecretForJoin = false
    private(set) var generatedSecret = ""
    var claimedChooserActive = false
    var inviteLink = ""
    var displayName = ""
    var vaultUsername = ""
    var vaultPassword = ""
    var vaultPasswordConfirmation = ""
    var vaultSecretPrepared = false
    /// The prepared request resumes with the protected stored secret; no entry is shown.
    var requiresSavedSecret = false
    /// The prepared request exists but no stored copy does, so the owner must re-enter the secret.
    var requiresEnteredSecret = false
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
        canContinueOwnerSetup && !generatedSecret.isEmpty
    }

    var canSubmitJoin: Bool {
        !origin.isEmpty && (!sharedSecret.isEmpty || hasPendingSecretForJoin)
    }

    var canSubmitVaultCreation: Bool {
        !vaultUsername.isEmpty && vaultPassword.utf8.count >= 12
            && vaultPassword == vaultPasswordConfirmation
            && vaultSecretReady
    }

    /// The secret the next vault creation will use: stored, re-entered, or generated for this invite.
    var vaultSecretReady: Bool {
        if requiresSavedSecret { return true }
        return requiresEnteredSecret ? !savedSecret.isEmpty : !generatedSecret.isEmpty
    }

    /// The saved-secret hint describes storage that already happened, so it is shown only on the resumed path.
    var savedSecretHintVisible: Bool { requiresSavedSecret }

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
        hasPendingSecretForJoin = false
    }

    mutating func clearSecrets() {
        ownerStep = .credentials
        setupToken = ""
        adminPassword = ""
        confirmation = ""
        sharedSecret = ""
        signInPassword = ""
        generatedSecret = ""
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

    /// Records what a resumed vault request holds. Without a stored copy, the owner must re-enter the secret.
    mutating func applyPreparedOwnerMaterial(storedSecret: Bool) {
        vaultSecretPrepared = true
        requiresSavedSecret = storedSecret
        requiresEnteredSecret = !storedSecret
    }

    mutating func clearInvite() {
        inviteEpoch &+= 1
        inviteLink = ""
        displayName = ""
        vaultUsername = ""
        vaultPassword = ""
        vaultPasswordConfirmation = ""
        generatedSecret = ""
        vaultSecretPrepared = false
        requiresSavedSecret = false
        requiresEnteredSecret = false
        savedSecret = ""
        inviteStage = .none
    }

    func isCurrentInviteEpoch(_ epoch: Int) -> Bool { inviteEpoch == epoch }
}
