import SwiftUI
import UIKit

/// Native first-owner and direct shared-secret admission. The generated Rust
/// client owns every request, proof, root import, and device credential.
@MainActor
struct SelfHostedEnrollmentView: View {
    let model: AppModel
    @Binding var isPresented: Bool
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.colorScheme) private var colorScheme
    @State private var enrollment = SelfHostedEnrollmentState()

    init(model: AppModel, isPresented: Binding<Bool>, inviteLink: String? = nil) {
        self.model = model
        _isPresented = isPresented
        var state = SelfHostedEnrollmentState()
        if let inviteLink { state.beginInviteRequest(link: inviteLink) }
        _enrollment = State(initialValue: state)
    }

    var body: some View {
        NavigationStack {
            Form {
                if enrollment.inviteStage != .none {
                    inviteSection
                } else if enrollment.claimedChooserActive {
                    claimedChooserSection
                } else if enrollment.signingIn {
                    signInSection
                } else if enrollment.joining {
                    joinSection
                } else if enrollment.ownerStep == .credentials {
                    ownerCredentialsSection
                } else {
                    ownerSecretSection
                }

                if let error = model.lastError {
                    Text(error)
                        .foregroundStyle(colors.Error)
                        .accessibilityIdentifier("self-hosted-enrollment-error")
                }
                if model.activity == .enrolling {
                    ProgressView().accessibilityIdentifier("self-hosted-enrollment-progress")
                }
            }
            .navigationTitle(enrollment.joining ? "Connect to Peppy" : "Set up Peppy")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") { cancel() }
                }
            }
        }
        .accessibilityIdentifier("self-hosted-enrollment-screen")
        .onDisappear { enrollment.clearSecrets() }
    }

    private var joinSection: some View {
        Group {
            Section("Connect to your server") {
                serverURLField
                SecureField("Vault shared secret", text: $enrollment.sharedSecret)
                    .textContentType(.password)
                    .accessibilityIdentifier("self-hosted-shared-secret")
                TextField(String(localized: "peppy.enrollment_vault_owner_label", table: "Peppy"), text: $enrollment.vaultOwner)
                    .textInputAutocapitalization(.never)
                    .accessibilityIdentifier("vault-owner-input")
            }
            Section {
                Button("Connect with shared secret") { submitJoin() }
                    .disabled(!enrollment.canSubmitJoin || isUnavailable)
                    .accessibilityIdentifier("self-hosted-manual-submit")
                modeButton
            }
        }
    }

    private var ownerCredentialsSection: some View {
        Group {
            Section("Set up your Peppy server") {
                serverURLField
                if enrollment.discoveredOrigin != nil {
                    setupModeCredentials
                }
            }
            Section {
                if enrollment.discoveredOrigin == nil {
                    Button("Check server") { discoverServer() }
                        .disabled(enrollment.origin.isEmpty || isUnavailable)
                        .accessibilityIdentifier("self-hosted-check-server")
                } else {
                    Button("Continue") {
                        enrollment.prepareOwnerSecret(using: generateHostedPassphrase)
                    }
                    .disabled(!enrollment.canContinueOwnerSetup || isUnavailable)
                    .accessibilityIdentifier("self-hosted-owner-continue")
                }
                modeButton
            }
        }
    }

    private var setupModeCredentials: some View {
        Group {
            if enrollment.setupCodeRequired {
                SecureField(String(localized: "peppy.enrollment_setup_code", table: "Peppy"), text: $enrollment.setupToken)
                    .textContentType(.oneTimeCode)
                    .accessibilityIdentifier("self-hosted-setup-token")
                Text(String(localized: "peppy.enrollment_setup_protected", table: "Peppy"))
                    .font(.footnote)
                    .accessibilityIdentifier("self-hosted-setup-mode-hint")
            } else {
                Text(String(localized: "peppy.enrollment_setup_open", table: "Peppy"))
                    .font(.footnote)
                    .accessibilityIdentifier("self-hosted-setup-mode-hint")
            }
            SecureField("Admin password", text: $enrollment.adminPassword)
                .textContentType(.newPassword)
                .accessibilityIdentifier("self-hosted-admin-password")
            TextField(String(localized: "peppy.enrollment_username_label", table: "Peppy"), text: $enrollment.ownerUsername)
                .textInputAutocapitalization(.never)
                .textContentType(.username)
                .accessibilityIdentifier("self-hosted-owner-username")
            SecureField("Confirm admin password", text: $enrollment.confirmation)
                .textContentType(.newPassword)
                .accessibilityIdentifier("self-hosted-admin-password-confirmation")
            Text(enrollment.setupCodeRequired
                ? "The setup code claims this server. Your admin password manages it; neither is your vault shared secret."
                : "Your admin password manages this server. It is not your vault shared secret.")
                .font(.footnote)
        }
    }

    private var ownerSecretSection: some View {
        Group {
            Section {
                Button("Create owner account") { submitOwnerClaim() }
                    .disabled(!enrollment.ownerClaimEnabled(
                        isBusy: model.activity != .idle,
                        sceneIsActive: scenePhase == .active
                    ))
                    .accessibilityIdentifier("self-hosted-owner-submit")
                Button("Back") {
                    enrollment.ownerStep = .credentials
                }
                .disabled(model.activity != .idle)
                modeButton
            }
        }
    }

    private var claimedChooserSection: some View {
        Section("Connect to your server") {
            Button(String(localized: "peppy.enrollment_signin_password_label", table: "Peppy")) {
                enrollment.signingIn = true
            }
            .accessibilityIdentifier("claimed-chooser-signin")
            Button(String(localized: "peppy.enrollment_signin_connect_label", table: "Peppy")) {
                enrollment.joining = true
            }
            .accessibilityIdentifier("claimed-chooser-connect")
            Button(String(localized: "peppy.enrollment_signin_invite_label", table: "Peppy")) {
                enrollment.beginInviteRequest()
            }
            .accessibilityIdentifier("claimed-chooser-invite")
        }
    }

    private var signInSection: some View {
        Group {
            Section(String(localized: "peppy.enrollment_signin_password_label", table: "Peppy")) {
                TextField(String(localized: "peppy.enrollment_username_label", table: "Peppy"), text: $enrollment.signInUsername)
                    .textInputAutocapitalization(.never).textContentType(.username).accessibilityIdentifier("signin-username-input")
                SecureField("Password", text: $enrollment.signInPassword)
                    .textContentType(.password).accessibilityIdentifier("signin-password-input")
            }
            Section {
                Button(String(localized: "peppy.enrollment_signin_cta", table: "Peppy")) { submitSignIn() }
                    .disabled(enrollment.signInPassword.isEmpty || isUnavailable).accessibilityIdentifier("signin-submit")
                Button("Back") { enrollment.signingIn = false }.accessibilityIdentifier("signin-back")
            }
        }
    }

    @ViewBuilder private var inviteSection: some View {
        switch enrollment.inviteStage {
        case .request: inviteRequestSection
        case .waiting: inviteWaitingSection
        case .approved: inviteApprovedSection
        case .denied: inviteTerminalSection(denied: true)
        case .expired: inviteTerminalSection(denied: false)
        case .none: EmptyView()
        }
    }

    private var inviteRequestSection: some View {
        Group {
            Section(String(localized: "peppy.enrollment_invite_request_headline", table: "Peppy")) {
                TextField(String(localized: "peppy.enrollment_invite_link_label", table: "Peppy"), text: $enrollment.inviteLink)
                    .keyboardType(.URL).textInputAutocapitalization(.never)
                    .accessibilityIdentifier("invite-link-input")
                TextField(String(localized: "peppy.enrollment_display_name_label", table: "Peppy"), text: $enrollment.displayName)
                    .textContentType(.name).accessibilityIdentifier("display-name-input")
            }
            Section {
                Button(String(localized: "peppy.enrollment_invite_request_cta", table: "Peppy")) { submitInviteRequest() }
                    .disabled(enrollment.inviteLink.isEmpty || enrollment.displayName.isEmpty || isUnavailable)
                    .accessibilityIdentifier("invite-submit")
                Button(String(localized: "peppy.enrollment_invite_cancel_cta", table: "Peppy")) { restartEnrollment() }
            }
        }
    }

    private var inviteWaitingSection: some View {
        Group {
            Section {
                Text(String(localized: "peppy.enrollment_invite_waiting_headline", table: "Peppy"))
                    .accessibilityAddTraits(.isHeader).accessibilityIdentifier("invite-waiting-headline")
                Text(String(localized: "peppy.enrollment_invite_waiting_body", table: "Peppy"))
                Text(String(localized: "peppy.enrollment_invite_waiting_hint", table: "Peppy")).font(.footnote)
                if model.activity == .enrolling { ProgressView().accessibilityIdentifier("invite-waiting-progress") }
            }
            Section {
                Button(String(localized: "peppy.enrollment_invite_cancel_cta", table: "Peppy")) { cancelInvite() }
                    .accessibilityIdentifier("invite-waiting-cancel")
            }
        }
        .task(id: enrollment.inviteStage == .waiting && scenePhase == .active) { await pollVaultRequest() }
    }

    private var inviteApprovedSection: some View {
        Group {
            Section(String(localized: "peppy.enrollment_invite_approved_headline", table: "Peppy")) {
                Text(String(localized: "peppy.enrollment_invite_approved_body", table: "Peppy"))
                TextField(String(localized: "peppy.enrollment_username_label", table: "Peppy"), text: $enrollment.vaultUsername)
                    .textInputAutocapitalization(.never).textContentType(.username).accessibilityIdentifier("vault-username-input")
                SecureField("Password", text: $enrollment.vaultPassword).textContentType(.newPassword).accessibilityIdentifier("vault-password-input")
                SecureField("Confirm password", text: $enrollment.vaultPasswordConfirmation).textContentType(.newPassword).accessibilityIdentifier("vault-password-confirmation")
                Text(String(localized: "peppy.enrollment_invite_approved_secure_hint", table: "Peppy")).font(.footnote)
                if enrollment.requiresEnteredSecret {
                    SecureField("Vault shared secret", text: $enrollment.savedSecret)
                        .textContentType(.password)
                        .accessibilityIdentifier("vault-shared-secret-input")
                }
                if enrollment.savedSecretHintVisible {
                    Text("peppy.owner_phone_secret_saved_hint", tableName: "Peppy").font(.footnote)
                }
            }
            .onAppear { prepareFreshVaultSecret() }
            Section {
                Button(String(localized: "peppy.enrollment_create_vault_cta", table: "Peppy")) { submitCreateVault() }
                    .disabled(!enrollment.canSubmitVaultCreation || isUnavailable).accessibilityIdentifier("create-vault-submit")
            }
        }
    }

    private func inviteTerminalSection(denied: Bool) -> some View {
        Group {
            Section {
                Text(String(localized: denied ? "peppy.enrollment_invite_denied_headline" : "peppy.enrollment_invite_expired_headline", table: "Peppy"))
                    .accessibilityAddTraits(.isHeader).accessibilityIdentifier(denied ? "invite-denied-headline" : "invite-expired-headline")
                Text(String(localized: denied ? "peppy.enrollment_invite_denied_body" : "peppy.enrollment_invite_expired_body", table: "Peppy")).foregroundStyle(colors.Error)
            }
            Section { Button(String(localized: "peppy.enrollment_invite_restart_cta", table: "Peppy")) { restartEnrollment() }.accessibilityIdentifier("invite-restart") }
        }
    }


    private var serverURLField: some View {
        TextField("Server URL", text: $enrollment.origin)
            .textInputAutocapitalization(.never)
            .keyboardType(.URL)
            .accessibilityIdentifier("self-hosted-server-url")
    }

    private var modeButton: some View {
        Button(enrollment.joining ? "Set up a new server instead" : "Already have a server?") {
            enrollment.switchMode()
        }
        .disabled(model.activity != .idle)
    }

    private var colors: PeppyColorScheme {
        PeppyTokens.colors(for: colorScheme)
    }

    private var isUnavailable: Bool {
        model.activity != .idle || scenePhase != .active
    }


    private func submitJoin() {
        let origin = enrollment.origin
        let secret = enrollment.sharedSecret
        let vaultOwner = enrollment.vaultOwner.isEmpty ? nil : enrollment.vaultOwner
        Task {
            await model.joinWithSharedSecret(origin: origin, vaultID: nil, vaultOwner: vaultOwner, passphrase: secret)
            finishIfReady()
        }
    }

    private func submitSignIn() {
        let origin = enrollment.origin
        let username = enrollment.signInUsername
        let password = enrollment.signInPassword
        Task {
            guard await model.signIn(origin: origin, username: username, password: password) else { return }
            enrollment.signInPassword = ""
            enrollment.signingIn = false
            enrollment.joining = true
        }
    }

    private func submitOwnerClaim() {
        let origin = enrollment.origin
        let setupToken = enrollment.setupToken
        let username = enrollment.ownerUsername
        let adminPassword = enrollment.adminPassword
        let passphrase = enrollment.generatedSecret
        Task {
            await model.claimInstance(
                origin: origin,
                setupToken: setupToken,
                username: username,
                adminPassword: adminPassword,
                passphrase: passphrase
            )
            finishIfReady()
        }
    }

    private func discoverServer() {
        let requestedOrigin = enrollment.origin
        let epoch = enrollment.beginDiscovery()
        Task {
            guard let discovery = await model.discoverSelfHostedInstance(origin: requestedOrigin), !enrollment.joining else { return }
            if discovery.claimed {
                enrollment.origin = discovery.origin
                enrollment.claimedChooserActive = true
                if let pending = await model.pendingVaultRequest(origin: discovery.origin) {
                    enrollment.displayName = pending.displayName
                    if pending.hasPreparedOwnerMaterial {
                        enrollment.applyPreparedOwnerMaterial(storedSecret: pending.hasStoredVaultSecret)
                        enrollment.inviteStage = .approved
                    } else {
                        enrollment.inviteStage = .waiting
                    }
                }
                return
            }
            enrollment.applyDiscovery(
                canonicalOrigin: discovery.origin,
                setupCodeRequired: discovery.setupCodeRequired,
                requestedOrigin: requestedOrigin,
                epoch: epoch
            )
        }
    }

    private func finishIfReady() {
        guard model.status.identity != nil else { return }
        enrollment.clearSecrets()
        isPresented = false
    }

    private func submitInviteRequest() {
        let link = enrollment.inviteLink
        let name = enrollment.displayName
        let expectedOrigin = enrollment.origin.isEmpty ? nil : enrollment.origin
        let epoch = enrollment.inviteEpoch
        Task {
            guard let progress = await model.beginVaultRequest(inviteLink: link, displayName: name, expectedOrigin: expectedOrigin), enrollment.isCurrentInviteEpoch(epoch) else { return }
            enrollment.origin = progress.origin
            enrollment.inviteStage = .waiting
        }
    }

    private func pollVaultRequest() async {
        var delay: UInt64 = 1
        let epoch = enrollment.inviteEpoch
        while enrollment.inviteStage == .waiting && scenePhase == .active && !Task.isCancelled {
            guard let progress = await model.vaultRequestStatus(origin: enrollment.origin) else {
                guard !Task.isCancelled, enrollment.isCurrentInviteEpoch(epoch), enrollment.inviteStage == .waiting, scenePhase == .active else { return }
                try? await Task.sleep(for: .seconds(delay))
                delay = min(delay == 1 ? 2 : delay == 2 ? 5 : 10, 10)
                continue
            }
            guard !Task.isCancelled, enrollment.isCurrentInviteEpoch(epoch), enrollment.inviteStage == .waiting, scenePhase == .active else { return }
            switch progress.state {
            case "approved":
                enrollment.inviteStage = .approved
            case "completed" where progress.hasPreparedOwnerMaterial:
                enrollment.applyPreparedOwnerMaterial(storedSecret: progress.hasStoredVaultSecret)
                enrollment.inviteStage = .approved
            case "completed":
                model.vaultRequestRecoveryRequired()
                enrollment.inviteStage = .expired
            case "denied": enrollment.inviteStage = .denied
            case "expired": enrollment.inviteStage = .expired
            default: break
            }
        }
    }

    private func submitCreateVault() {
        let origin = enrollment.origin
        let username = enrollment.vaultUsername
        let password = enrollment.vaultPassword
        let secret = vaultSecretForSubmission()
        Task {
            await model.createVault(origin: origin, username: username, adminPassword: password, passphrase: secret)
            finishIfReady()
        }
    }

    /// Stored secrets are resolved natively (empty value). A prepared request without a stored copy uses the re-entered secret.
    private func vaultSecretForSubmission() -> String {
        if enrollment.requiresEnteredSecret { return enrollment.savedSecret }
        if enrollment.requiresSavedSecret { return "" }
        prepareFreshVaultSecret()
        return enrollment.generatedSecret
    }

    /// A fresh approved invite generates its vault secret up front so the create button can enable.
    /// Resumed requests keep their stored or re-entered material.
    private func prepareFreshVaultSecret() {
        guard !enrollment.vaultSecretPrepared, !enrollment.requiresSavedSecret, !enrollment.requiresEnteredSecret else { return }
        enrollment.prepareVaultSecret(using: generateHostedPassphrase)
    }

    private func cancelInvite() {
        if enrollment.vaultSecretPrepared || enrollment.requiresSavedSecret {
            model.vaultRequestRecoveryRetained()
            return
        }
        model.cancelVaultRequest(origin: enrollment.origin)
        restartEnrollment()
    }

    private func restartEnrollment() {
        enrollment.clearInvite()
        enrollment.claimedChooserActive = !enrollment.origin.isEmpty
    }

    private func cancel() {
        enrollment.clearSecrets()
        isPresented = false
    }
}
