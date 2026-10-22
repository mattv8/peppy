import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

@MainActor
struct PhoneEnrollmentView: View {
    let model: AppModel
    @Binding var isPresented: Bool
    @State private var status: PhoneEnrollmentStatus?
    @State private var sharedSecret = ""
    @State private var coordinator = PhoneEnrollmentAttemptCoordinator()
    @State private var inviteLink: String?
    @State private var operation: Task<Void, Never>?
    @State private var claimError: String?
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        NavigationStack {
            if let inviteLink {
                SelfHostedEnrollmentView(model: model, isPresented: $isPresented, inviteLink: inviteLink)
            } else {
                VStack(spacing: 16) {
                QRScannerView { qr in startScan(qr) }
                    .frame(height: 300).clipShape(RoundedRectangle(cornerRadius: 16))
                    .allowsHitTesting(!coordinator.isStarting && !coordinator.isConfirming)
                    .accessibilityIdentifier("phone-enrollment-scanner")
                if coordinator.isStarting {
                    operationStatus("peppy.production_pairing_checking", id: "phone-enrollment-checking")
                }
                if let status {
                    if let code = status.verificationCode {
                        Text(code).font(.title.monospaced())
                            .accessibilityIdentifier("phone-enrollment-sas")
                        if status.awaitingOwner { Text("Compare this code with the owner before continuing.") }
                    }
                    if status.awaitingOwner {
                        Text("Waiting for the owner to approve this phone.").accessibilityIdentifier("phone-enrollment-waiting")
                        progressGlyph
                    } else if coordinator.isConfirming {
                        operationStatus("peppy.production_pairing_connecting", id: "phone-enrollment-connecting")
                    } else {
                        if status.requiresSharedSecret && !coordinator.canRetryConfirmation && !coordinator.needsRescan {
                            SecureField("Vault shared secret", text: $sharedSecret)
                                .textContentType(.password).accessibilityIdentifier("phone-enrollment-shared-secret")
                            Button("Confirm connection") {
                                guard let attempt = coordinator.beginManualConfirmation() else { return }
                                let secret = sharedSecret
                                sharedSecret = ""
                                startConfirmation(attempt, sharedSecret: secret)
                            }
                            .disabled(sharedSecret.isEmpty || coordinator.isConfirming)
                            .accessibilityIdentifier("phone-enrollment-confirm")
                        }
                    }
                } else {
                    Text("Scan the QR code from your Peppy owner device.")
                }
                if let error = model.lastError ?? claimError, !coordinator.isConfirming {
                    Text(error).foregroundStyle(.red).accessibilityIdentifier("phone-enrollment-error")
                    Button(coordinator.canRetryConfirmation ? "Try again" : "Scan again") {
                        if coordinator.canRetryConfirmation {
                            if let attempt = coordinator.retryConfirmation() { startConfirmation(attempt, sharedSecret: nil) }
                        } else {
                            restart()
                        }
                    }
                        .accessibilityIdentifier("phone-enrollment-retry")
                }
                }
            .padding().navigationTitle("Connect phone")
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { cancel(); isPresented = false } } }
            }
        }
        .accessibilityIdentifier("phone-enrollment-screen")
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { sharedSecret = "" }
        }
        .onDisappear { cancel() }
    }

    private func startScan(_ qr: String) {
        guard let attempt = coordinator.beginScan() else { return }
        claimError = nil
        operation = Task { await scan(qr, attempt: attempt) }
    }

    private func scan(_ qr: String, attempt: UUID) async {
        if await model.isVaultInviteLink(qr) {
            guard !Task.isCancelled, coordinator.activeAttempt == attempt else { return }
            inviteLink = qr
            return
        }
        guard !Task.isCancelled, coordinator.activeAttempt == attempt else { return }
        guard let scanned = await model.beginPhoneEnrollment(qr: qr) else {
            if !Task.isCancelled, coordinator.activeAttempt == attempt {
                claimError = model.lastError ?? "Could not start phone enrollment. Scan the code again."
                coordinator.cancel()
            }
            return
        }
        guard !Task.isCancelled, coordinator.activeAttempt == attempt else { return }
        let outcome = await coordinator.run(
            initial: scanned,
            poll: { await model.phoneEnrollmentStatus() },
            wait: { try? await Task.sleep(for: .seconds(3)) },
            onStatus: { status = $0 },
            confirm: {
                announceConnecting()
                return await model.confirmPhoneEnrollment(sharedSecret: nil)
            }
        )
        if outcome == .succeeded { isPresented = false }
    }

    private func startConfirmation(_ attempt: UUID, sharedSecret: String?) {
        if status?.requiresSharedSecret == false { announceConnecting() }
        operation = Task { await confirm(attempt, sharedSecret: sharedSecret) }
    }

    private func confirm(_ attempt: UUID, sharedSecret: String?) async {
        let outcome = await model.confirmPhoneEnrollment(sharedSecret: sharedSecret)
        guard coordinator.finishedConfirmation(for: attempt, outcome: outcome) else { return }
        isPresented = false
    }

    private func restart() {
        cancel()
        status = nil
    }

    private func cancel() {
        sharedSecret = ""
        operation?.cancel()
        operation = nil
        coordinator.cancel()
        model.cancelPhoneEnrollment()
    }

    private func announceConnecting() {
        #if canImport(UIKit)
        UIAccessibility.post(notification: .announcement, argument: String(localized: "peppy.production_pairing_connecting", table: "Peppy"))
        #endif
    }

    @ViewBuilder private var progressGlyph: some View {
        if reduceMotion {
            Image(systemName: "hourglass").accessibilityHidden(true)
        } else {
            ProgressView().accessibilityHidden(true)
        }
    }

    @ViewBuilder private func operationStatus(_ key: String, id: String) -> some View {
        VStack {
            progressGlyph
            Text(NSLocalizedString(key, tableName: "Peppy", comment: ""))
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(NSLocalizedString(key, tableName: "Peppy", comment: "")))
        .accessibilityIdentifier(id)
    }
}
