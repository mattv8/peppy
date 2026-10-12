import Foundation

struct SelfHostedEnrollmentState {
    enum OwnerStep {
        case credentials
        case vaultSecret
    }

    var joining = false
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
    var adminPassword = ""
    var confirmation = ""
    var sharedSecret = ""
    private(set) var generatedSecret = ""
    var acknowledgedSecretSave = false
    var revealsGeneratedSecret = false

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
        generatedSecret = ""
        acknowledgedSecretSave = false
        revealsGeneratedSecret = false
        invalidateDiscovery()
    }
}
