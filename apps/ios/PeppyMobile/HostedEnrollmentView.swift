import SwiftUI
import UIKit
#if canImport(PeppyNative)
import PeppyNative
#endif

/// Real native hosted enrollment with production account client and GoogleHostedSignIn SDK.
/// Presents phases: sign-in, billing, passphrase generation, confirmation, and vault provisioning.
/// Keeps secrets transient in String bindings; never logs passphrases or generated values.
@MainActor
struct HostedEnrollmentView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    
    let model: AppModel
    let onPairExisting: () -> Void
    
    @State private var client: HostedAccountClient?
    @State private var phase: EnrollmentPhase = .loading
    @State private var account: NativeHostedAccount?
    @State private var provisioning: NativeHostedProvisioning?
    
    // Transient secrets: never saved, logged or shared
    @State private var generatedPassphrase = ""
    @State private var customPassphrase = ""
    @State private var confirmationPassphrase = ""
    @State private var provisioningPassphrase = ""
    @State private var acknowledgement = false
    @State private var revealPassphrase = false
    
    @State private var error: HostedAccountClientError?
    @State private var localError: String?
    @State private var uiGeneration = 0
    @State private var task: Task<Void, Never>?
    @State private var taskGeneration = 0
    @State private var sensitiveContentHidden = false
    @State private var refreshWhenActive = false
    
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    content
                        .padding(24)
                        .frame(maxWidth: 560, alignment: .leading)
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") { cancel() }.accessibilityIdentifier("enrollment-cancel")
                }
            }
        }
        .task { await initialize() }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active {
                sensitiveContentHidden = true
                confirmationPassphrase = ""
            } else {
                sensitiveContentHidden = false
                if refreshWhenActive {
                    refreshWhenActive = false
                    launchTask { await refreshAccount() }
                }
            }
        }
        .onDisappear { task?.cancel(); clearSecrets() }
    }
    
    @ViewBuilder private var content: some View {
        if sensitiveContentHidden && (phase == .passphrase || phase == .confirm) {
            ProgressView().accessibilityIdentifier("enrollment-sensitive-content-hidden")
        } else {
            switch phase {
        case .loading:
            ProgressView().accessibilityIdentifier("enrollment-loading")
        case .signIn:
            signInView
        case .checking:
            checkingView
        case .billing:
            billingView
        case .passphrase:
            passphraseView
        case .confirm:
            confirmView
        case .provisioning:
            provisioningView
        case .existing:
            existingVaultView
            case .error:
                errorView
            }
        }
    }
    
    private var signInView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.hosted_sign_in_headline", tableName: "Peppy").font(.title.bold())
            Text("peppy.hosted_sign_in_body", tableName: "Peppy")
            
            if GoogleHostedSignIn.isAvailable() {
                Button {
                    launchTask { await signInWithGoogle() }
                } label: {
                    Text("peppy.hosted_sign_in_google", tableName: "Peppy")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(colors.Accent)
                .foregroundStyle(colors.AccentText)
                .accessibilityIdentifier("enrollment-signin-google")
                .disabled(task != nil)
            } else {
                Text("peppy.production_provider_unconfigured", tableName: "Peppy")
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-config-unavailable")
            }
            
            if let err = error {
                Text(errorMessage(err))
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-signin-error")
            }
        }
        .accessibilityIdentifier("enrollment-signin-screen")
    }
    
    private var checkingView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.hosted_account_checking", tableName: "Peppy").font(.title.bold())
            ProgressView().accessibilityIdentifier("enrollment-checking-progress")
        }
        .accessibilityIdentifier("enrollment-checking-screen")
    }
    
    private var billingView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.hosted_subscribe_headline", tableName: "Peppy").font(.title.bold())
            Text("peppy.hosted_subscribe_body", tableName: "Peppy")
            
            Button {
                openBillingPage()
            } label: {
                Text("peppy.hosted_subscribe_cta", tableName: "Peppy")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(colors.Accent)
            .foregroundStyle(colors.AccentText)
            .accessibilityIdentifier("enrollment-billing-button")
            
            Text("peppy.production_billing_body", tableName: "Peppy")
                .font(.footnote)
                .foregroundStyle(colors.TextSecondary)
            
            if let err = error {
                Text(errorMessage(err))
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-billing-error")
            }
            switchAccountButton
        }
        .accessibilityIdentifier("enrollment-billing-screen")
    }
    
    private var passphraseView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.passphrase_create_headline", tableName: "Peppy").font(.title.bold())
            Text("peppy.passphrase_create_body", tableName: "Peppy")
            Text("peppy.passphrase_irrecoverable_warn", tableName: "Peppy").font(.footnote)
            
            passphraseField("passphrase_field", text: $generatedPassphrase)
            
            Button {
                generatedPassphrase = generateHostedPassphrase()
                customPassphrase = ""
                acknowledgement = false
                localError = nil
            } label: {
                Text("peppy.passphrase_generate_cta", tableName: "Peppy")
            }
            .accessibilityIdentifier("enrollment-passphrase-generate")
            .disabled(task != nil)
            
            Button {
                let value = generatedPassphrase
                UIPasteboard.general.setItems([["public.utf8-plain-text": value]], options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(60)])
            } label: {
                Text("peppy.passphrase_copy", tableName: "Peppy")
            }
            .accessibilityIdentifier("enrollment-passphrase-copy")
            .disabled(task != nil || generatedPassphrase.isEmpty)
            
            Button {
                revealPassphrase.toggle()
            } label: {
                Text(LocalizedStringKey(revealPassphrase ? "peppy.passphrase_hide" : "peppy.passphrase_show"), tableName: "Peppy")
            }
            .accessibilityIdentifier("enrollment-passphrase-reveal")
            .disabled(task != nil)
            
            Toggle(isOn: $acknowledgement) {
                Text("peppy.passphrase_ack_label", tableName: "Peppy")
            }
            .accessibilityIdentifier("enrollment-passphrase-ack")
            .disabled(task != nil)
            
            Button {
                advancePassphrase()
            } label: {
                Text("peppy.passphrase_create_submit", tableName: "Peppy")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(colors.Accent)
            .foregroundStyle(colors.AccentText)
            .accessibilityIdentifier("enrollment-passphrase-submit")
            .disabled(!isValidPassphrase() || !acknowledgement || task != nil)
            
            if let localErr = localError {
                Text(LocalizedStringKey("peppy.\(localErr)"), tableName: "Peppy")
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-passphrase-error")
            }
            switchAccountButton
        }
        .accessibilityIdentifier("enrollment-passphrase-screen")
    }
    
    private var confirmView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.passphrase_confirm_headline", tableName: "Peppy").font(.title.bold())
            Text("peppy.passphrase_confirm_body", tableName: "Peppy")
            
            passphraseField("passphrase_confirm_field", text: $confirmationPassphrase)
            
            Button {
                confirmPassphrase()
            } label: {
                Text("peppy.continue", tableName: "Peppy")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(colors.Accent)
            .foregroundStyle(colors.AccentText)
            .accessibilityIdentifier("enrollment-confirm-submit")
            .disabled(confirmationPassphrase.isEmpty || task != nil)
            
            if let localErr = localError {
                Text("peppy.\(localErr)", tableName: "Peppy")
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-confirm-error")
            }
        }
        .accessibilityIdentifier("enrollment-confirm-screen")
    }
    
    private var provisioningView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.provisioning_headline", tableName: "Peppy").font(.title.bold())
            ProgressView().accessibilityIdentifier("enrollment-provisioning-progress")
            
            if let err = error {
                Text(errorMessage(err))
                    .foregroundStyle(colors.Error)
                    .accessibilityIdentifier("enrollment-provisioning-error")
                
                Button {
                    provisioningPassphrase = ""
                    confirmationPassphrase = ""
                    launchTask { await refreshAccount() }
                } label: {
                    Text("peppy.try_again", tableName: "Peppy")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(colors.Accent)
                .foregroundStyle(colors.AccentText)
                .accessibilityIdentifier("enrollment-provisioning-retry")
            }
        }
        .accessibilityIdentifier("enrollment-provisioning-screen")
    }
    
    private var existingVaultView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.hosted_join_headline", tableName: "Peppy").font(.title.bold())
            Text("peppy.hosted_join_body", tableName: "Peppy")
            
            Button {
                onPairExisting()
                dismiss()
            } label: {
                Text("peppy.production_join_cta", tableName: "Peppy")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(colors.Accent)
            .foregroundStyle(colors.AccentText)
            .accessibilityIdentifier("enrollment-pair-existing")
            Link(destination: URL(string: "https://peppy.pro/account")!) {
                Text("peppy.production_manage_account", tableName: "Peppy")
            }.accessibilityIdentifier("enrollment-existing-manage-account")
        }
        .accessibilityIdentifier("enrollment-existing-vault-screen")
    }
    
    private var errorView: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("peppy.hosted_sign_in_headline", tableName: "Peppy").font(.title.bold())
            if let err = error {
                Text(errorMessage(err))
                    .foregroundStyle(colors.Error)
            }
            
            Button {
                phase = .signIn
                error = nil
            } label: {
                Text("peppy.back", tableName: "Peppy")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(colors.Accent)
            .foregroundStyle(colors.AccentText)
            .accessibilityIdentifier("enrollment-error-back")
            switchAccountButton
        }
        .accessibilityIdentifier("enrollment-error-screen")
    }
    
    private var colors: PeppyColorScheme {
        PeppyTokens.colors(for: colorScheme)
    }
    
    private func passphraseField(_ key: String, text binding: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(LocalizedStringKey("peppy.\(key)"), tableName: "Peppy").font(.subheadline)
            if revealPassphrase {
                TextField("", text: binding)
            } else {
                SecureField("", text: binding)
            }
        }
        .textContentType(.password)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .privacySensitive()
        .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier(key == "passphrase_confirm_field" ? "enrollment-confirm-passphrase-field" : "enrollment-passphrase-field")
        .disabled(task != nil)
    }
    
    // MARK: - Logic
    
    private func initialize() async {
        do {
            let client = try HostedAccountClient()
            self.client = client
            await refreshAccount()
        } catch {
            self.error = error as? HostedAccountClientError ?? .unavailable
            phase = .error
        }
    }
    
    private func refreshAccount() async {
        guard let client else { return }
        phase = .checking
        error = nil
        
        do {
            let gen = uiGeneration
            let acct = try await client.account()
            guard gen == uiGeneration else { return }
            self.account = acct
            let pending = try await client.pendingProvisioning()
            guard gen == uiGeneration else { return }
            self.provisioning = pending
            
            if let pending, let vaultID = acct.vaultId {
                phase = vaultID == pending.view().vaultId ? .confirm : .existing
            } else if pending != nil {
                phase = .confirm
            } else if acct.vaultId != nil {
                phase = .existing
            } else if acct.access == "read_write" {
                phase = .passphrase
                if generatedPassphrase.isEmpty { generatedPassphrase = generateHostedPassphrase() }
            } else {
                phase = .billing
            }
        } catch let err as HostedAccountClientError {
            self.error = err
            if err == .sessionExpired {
                phase = .signIn
            } else {
                phase = .error
            }
        } catch {
            self.error = .unavailable
            phase = .error
        }
    }
    
    private func signInWithGoogle() async {
        guard let client else { return }
        let gen = uiGeneration
        error = nil
        
        do {
            let googleSignIn = GoogleHostedSignIn()
            guard let viewController = UIApplication.shared.connectedScenes
                .compactMap({ $0 as? UIWindowScene })
                .flatMap({ $0.windows })
                .first(where: { $0.isKeyWindow })?
                .rootViewController else {
                throw GoogleHostedSignIn.Error.unavailable
            }
            
            self.account = try await googleSignIn.signIn(client: client, presenting: viewController)
            guard gen == uiGeneration else { return }
            
            phase = .checking
            await refreshAccount()
        } catch let err as GoogleHostedSignIn.Error {
            guard gen == uiGeneration else { return }
            if err == .cancelled {
                phase = .signIn
            } else {
                error = .unavailable
                phase = .error
            }
        } catch {
            guard gen == uiGeneration else { return }
            self.error = .unavailable
            phase = .error
        }
    }
    
    private func openBillingPage() {
        if let url = URL(string: "https://peppy.pro/account/subscribe") {
            UIApplication.shared.open(url)
        }
        refreshWhenActive = true
    }
    
    private func advancePassphrase() {
        guard !generatedPassphrase.isEmpty, isValidPassphrase(), acknowledgement else { return }
        localError = nil
        phase = .confirm
        confirmationPassphrase = ""
    }
    
    private func confirmPassphrase() {
        let phrase = confirmationPassphrase
        guard !phrase.isEmpty else {
            localError = "passphrase_mismatch"
            return
        }
        guard provisioning != nil || phrase == generatedPassphrase else {
            localError = "passphrase_mismatch"
            return
        }
        localError = nil
        confirmationPassphrase = ""
        provisioningPassphrase = phrase
        phase = .provisioning
        launchTask { await provisionVault(passphrase: phrase) }
    }
    
    private func provisionVault(passphrase phrase: String) async {
        guard let client, !phrase.isEmpty else { return }
        let gen = uiGeneration
        error = nil
        
        do {
            _ = try await client.prepareVault(passphrase: phrase)
            guard gen == uiGeneration else { return }
            
            let data = try await client.completeVault(passphrase: phrase)
            guard gen == uiGeneration else { return }
            
            let imported = try await model.session.importCredential(data)
            guard gen == uiGeneration,
                  let identity = imported.identity else {
                throw HostedAccountClientError.invalidResponse
            }
            
            try await client.acknowledgeEnrollment(vaultID: identity.vaultId, deviceID: identity.deviceId)
            await model.unlock(passphrase: phrase)
            await model.load()
            guard gen == uiGeneration else { return }
            clearSecrets()
            dismiss()
        } catch let err as HostedAccountClientError {
            guard gen == uiGeneration else { return }
            self.error = err
            if err == .wrongPassphrase {
                provisioningPassphrase = ""
                confirmationPassphrase = ""
                localError = "passphrase_mismatch"
                phase = .confirm
            } else if err == .sessionExpired {
                phase = .signIn
            } else if err == .entitlementRequired {
                phase = .billing
            }
        } catch {
            guard gen == uiGeneration else { return }
            self.error = .unavailable
        }
    }
    
    private func cancel() {
        uiGeneration &+= 1
        task?.cancel()
        clearSecrets()
        dismiss()
    }

    private var switchAccountButton: some View {
        Button { launchTask { await switchAccount() } } label: {
            Text("peppy.settings_server_sign_out", tableName: "Peppy")
        }
        .accessibilityIdentifier("enrollment-switch-account")
        .disabled(task != nil)
    }

    private func switchAccount() async {
        uiGeneration &+= 1
        do {
            try await client?.signOut()
            clearSecrets()
            account = nil
            provisioning = nil
            error = nil
            phase = .signIn
        } catch {
            self.error = .unavailable
            phase = .error
        }
    }

    private func launchTask(_ operation: @escaping @MainActor () async -> Void) {
        taskGeneration &+= 1
        let generation = taskGeneration
        task = Task {
            defer { if taskGeneration == generation { task = nil } }
            await operation()
        }
    }
    
    private func clearSecrets() {
        generatedPassphrase = ""
        customPassphrase = ""
        confirmationPassphrase = ""
        provisioningPassphrase = ""
        acknowledgement = false
        revealPassphrase = false
    }
    
    private func isValidPassphrase() -> Bool {
        let value = generatedPassphrase
        return !value.isEmpty && hostedPassphraseAcceptable(passphrase: value)
    }
    
    private func errorMessage(_ error: HostedAccountClientError) -> String {
        switch error {
        case .sessionExpired:
            return NSLocalizedString("peppy.production_session_expired", tableName: "Peppy", bundle: .main, comment: "")
        case .unavailable:
            return NSLocalizedString("peppy.production_hosted_unavailable", tableName: "Peppy", bundle: .main, comment: "")
        case .wrongAccount:
            return NSLocalizedString("peppy.production_account_mismatch", tableName: "Peppy", bundle: .main, comment: "")
        case .wrongPassphrase:
            return NSLocalizedString("peppy.passphrase_mismatch", tableName: "Peppy", bundle: .main, comment: "")
        case .entitlementRequired:
            return NSLocalizedString("peppy.production_billing_pending", tableName: "Peppy", bundle: .main, comment: "")
        case .provisioningInProgress:
            return NSLocalizedString("peppy.production_provision_resume", tableName: "Peppy", bundle: .main, comment: "")
        case .invalidResponse:
            return NSLocalizedString("peppy.production_provision_retry", tableName: "Peppy", bundle: .main, comment: "")
        }
    }
}

enum EnrollmentPhase: Equatable {
    case loading
    case signIn
    case checking
    case billing
    case passphrase
    case confirm
    case provisioning
    case existing
    case error
}
