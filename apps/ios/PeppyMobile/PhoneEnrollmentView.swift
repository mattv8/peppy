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

    var body: some View {
        NavigationStack {
            if let inviteLink {
                SelfHostedEnrollmentView(model: model, isPresented: $isPresented, inviteLink: inviteLink)
            } else {
                VStack(spacing: 16) {
                QRScannerView { qr in startScan(qr) }
                    .frame(height: 300).clipShape(RoundedRectangle(cornerRadius: 16))
                    .accessibilityIdentifier("phone-enrollment-scanner")
                if let status {
                    if let code = status.verificationCode {
                        Text(code).font(.title.monospaced())
                            .accessibilityIdentifier("phone-enrollment-sas")
                        if status.awaitingOwner { Text("Compare this code with the owner before continuing.") }
                    }
                    if status.awaitingOwner {
                        Text("Waiting for the owner to approve this phone.").accessibilityIdentifier("phone-enrollment-waiting")
                        ProgressView()
                    } else if coordinator.isConfirming && !status.requiresSharedSecret {
                        VStack {
                            ProgressView()
                            Text("peppy.production_pairing_connecting", tableName: "Peppy")
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel(Text("peppy.production_pairing_connecting", tableName: "Peppy"))
                        .accessibilityIdentifier("phone-enrollment-connecting")
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
                if let error = model.lastError, !coordinator.isConfirming {
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
        .onDisappear { cancel() }
    }

    private func startScan(_ qr: String) {
        guard let attempt = coordinator.beginScan() else { return }
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
            if !Task.isCancelled, coordinator.activeAttempt == attempt { coordinator.cancel() }
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
}
