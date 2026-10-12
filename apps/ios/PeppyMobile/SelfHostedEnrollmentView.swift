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

    var body: some View {
        NavigationStack {
            Form {
                if enrollment.joining {
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
        .onChange(of: scenePhase) { _, phase in
            if phase != .active {
                enrollment.revealsGeneratedSecret = false
            }
        }
        .onDisappear { enrollment.clearSecrets() }
    }

    private var joinSection: some View {
        Group {
            Section("Connect to your server") {
                serverURLField
                SecureField("Vault shared secret", text: $enrollment.sharedSecret)
                    .textContentType(.password)
                    .accessibilityIdentifier("self-hosted-shared-secret")
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
            Section("Save your vault shared secret") {
                Text("This separate six-word secret unlocks your encrypted vault. Save it somewhere safe before creating the owner account.")
                    .font(.footnote)
                generatedSecretDisplay

                Button(enrollment.revealsGeneratedSecret ? "Hide shared secret" : "Reveal shared secret") {
                    enrollment.revealsGeneratedSecret.toggle()
                }
                .disabled(isUnavailable)
                .accessibilityIdentifier("self-hosted-secret-reveal")

                Button("Copy shared secret") {
                    UIPasteboard.general.setItems(
                        [["public.utf8-plain-text": enrollment.generatedSecret]],
                        options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(60)]
                    )
                }
                .disabled(isUnavailable)
                .accessibilityIdentifier("self-hosted-secret-copy")

                Toggle("I saved this shared secret.", isOn: $enrollment.acknowledgedSecretSave)
                    .disabled(isUnavailable)
                    .accessibilityIdentifier("self-hosted-secret-ack")
            }
            Section {
                Button("Create owner account") { submitOwnerClaim() }
                    .disabled(!enrollment.ownerClaimEnabled(
                        isBusy: model.activity != .idle,
                        sceneIsActive: scenePhase == .active
                    ))
                    .accessibilityIdentifier("self-hosted-owner-submit")
                Button("Back") {
                    enrollment.ownerStep = .credentials
                    enrollment.revealsGeneratedSecret = false
                }
                .disabled(model.activity != .idle)
                modeButton
            }
        }
    }

    private var generatedSecretDisplay: some View {
        Group {
            if enrollment.revealsGeneratedSecret {
                Text(enrollment.generatedSecret)
                    .textSelection(.enabled)
                    .accessibilityLabel("Generated vault shared secret")
            } else {
                Text(concealedSecret)
                    .accessibilityLabel("Generated vault shared secret hidden")
            }
        }
        .font(.body.monospaced())
        .privacySensitive()
        .accessibilityIdentifier("self-hosted-generated-secret")
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

    private var concealedSecret: String {
        String(repeating: "•••• ", count: 5) + "••••"
    }

    private func submitJoin() {
        let origin = enrollment.origin
        let secret = enrollment.sharedSecret
        Task {
            await model.joinWithSharedSecret(origin: origin, vaultID: nil, passphrase: secret)
            finishIfReady()
        }
    }

    private func submitOwnerClaim() {
        let origin = enrollment.origin
        let setupToken = enrollment.setupToken
        let adminPassword = enrollment.adminPassword
        let passphrase = enrollment.generatedSecret
        Task {
            await model.claimInstance(
                origin: origin,
                setupToken: setupToken,
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
            guard let discovery = await model.discoverSelfHostedInstance(origin: requestedOrigin),
                  !enrollment.joining else { return }
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

    private func cancel() {
        enrollment.clearSecrets()
        isPresented = false
    }
}
