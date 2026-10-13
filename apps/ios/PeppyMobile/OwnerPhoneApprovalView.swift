import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit
#if canImport(PeppyNative)
import PeppyNative
#endif

@MainActor
struct OwnerPhoneApprovalView: View {
    let host: OwnerPhoneApprovalHost
    @Binding var isPresented: Bool
    /// The Phase-B owner supplies a device-authenticated, transient reveal surface.
    let revealSecret: () async -> String?

    @State private var status: OwnerPhoneStatus?
    @State private var qr: String?
    @State private var codesMatch = false
    @State private var errorCode: String?
    @State private var isBusy = false
    @State private var isRotating = false
    @State private var showingApprovalPrompt = false
    @State private var promptSecret = ""
    @State private var promptContinuation: CheckedContinuation<String?, Never>?
    @State private var revealedSecret: String?
    @State private var requestedMethod = OwnerPhoneMethod.transfer
    @Environment(\.scenePhase) private var scenePhase

    init(host: OwnerPhoneApprovalHost, isPresented: Binding<Bool>, revealSecret: @escaping () async -> String? = { nil }) {
        self.host = host
        _isPresented = isPresented
        self.revealSecret = revealSecret
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    stageContent
                    if let errorCode {
                        Text(errorMessage(errorCode))
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("owner-phone-error")
                        Button(String(localized: "peppy.try_again", table: "Peppy")) { Task { await retry() } }
                    }
                }
                .onChange(of: errorCode) { _, value in
                    guard let value else { return }
                    UIAccessibility.post(notification: .announcement, argument: errorMessage(value))
                }
                .frame(maxWidth: 460)
                .padding(24)
            }
            .navigationTitle(Text("peppy.owner_phone_nav_title", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(String(localized: "peppy.cancel", table: "Peppy")) { Task { await cancel() } }
                        .accessibilityIdentifier("owner-phone-cancel")
                }
            }
        }
        .accessibilityIdentifier("owner-phone-screen")
        .task { await start() }
        .task(id: status?.stage) { await pollUntilSettled() }
        .sheet(isPresented: $showingApprovalPrompt) { approvalPrompt }
        .sheet(isPresented: Binding(get: { revealedSecret != nil }, set: { if !$0 { revealedSecret = nil } })) {
            NavigationStack {
                Text(revealedSecret ?? "")
                    .font(.body.monospaced())
                    .textSelection(.enabled)
                    .privacySensitive()
                    .padding(24)
                    .accessibilityIdentifier("owner-phone-reveal")
                    .navigationTitle(Text("peppy.owner_phone_secret_reveal_cta", tableName: "Peppy"))
                    .toolbar { ToolbarItem(placement: .topBarTrailing) { Button(String(localized: "peppy.owner_phone_secret_hide_cta", table: "Peppy")) { revealedSecret = nil } } }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            // The revealed secret must not stay on screen while the app is inactive or in the app switcher.
            if phase != .active { revealedSecret = nil }
        }
        .onDisappear { resumePrompt(with: nil) }
    }

    @ViewBuilder private var stageContent: some View {
        switch status?.stage {
        case .waiting:
            Text("peppy.owner_phone_waiting_headline", tableName: "Peppy").font(.title2.bold())
            Text("peppy.owner_phone_waiting_body", tableName: "Peppy")
            if isRotating {
                ProgressView()
                    .accessibilityLabel(Text("peppy.production_pairing_connecting", tableName: "Peppy"))
                    .accessibilityIdentifier("owner-phone-qr-rotating")
            }
            if let qr, let image = qrImage(qr) {
                Image(uiImage: image).interpolation(.none).resizable().scaledToFit()
                    .frame(maxWidth: 320).clipShape(RoundedRectangle(cornerRadius: 16))
                    .accessibilityLabel(Text("peppy.owner_phone_qr_accessibility", tableName: "Peppy"))
                    .accessibilityIdentifier("owner-phone-qr")
            }
            Text(countdownText).font(.footnote).accessibilityIdentifier("owner-phone-countdown")
            Text("peppy.owner_phone_waiting_hint", tableName: "Peppy").font(.footnote)
            Button { Task { await revealSharedSecret() } } label: {
                Text("peppy.owner_phone_shared_secret_cta", tableName: "Peppy")
            }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("owner-phone-shared-secret")
        case .verify:
            Text("peppy.owner_phone_verify_headline", tableName: "Peppy").font(.title2.bold())
            Text("peppy.owner_phone_verify_body", tableName: "Peppy")
            if let sas = status?.sas {
                Text(sas).font(.title.monospaced())
                    .accessibilityLabel(Text(verbatim: "\(String(localized: "peppy.production_pairing_sas_label", table: "Peppy")) \(sas)"))
                    .accessibilityIdentifier("owner-phone-sas")
            }
            Toggle(String(localized: "peppy.owner_phone_codes_match", table: "Peppy"), isOn: $codesMatch)
                .frame(minHeight: 44).accessibilityIdentifier("owner-phone-codes-match")
            Button(String(localized: "peppy.owner_phone_approve_cta", table: "Peppy")) { Task { await approve() } }
                .disabled(!codesMatch || isBusy).accessibilityIdentifier("owner-phone-approve")
            if isBusy { connectingSpinner }
        case .approved:
            Text("peppy.owner_phone_approved_body", tableName: "Peppy").font(.title2.bold())
                .accessibilityIdentifier("owner-phone-approved")
            connectingSpinner
        case .connected:
            Text("peppy.owner_phone_connected_headline", tableName: "Peppy").font(.title2.bold())
                .accessibilityAddTraits(.isHeader).accessibilityIdentifier("owner-phone-connected")
            Text("peppy.owner_phone_connected_body", tableName: "Peppy")
            Button(String(localized: "peppy.owner_phone_done_cta", table: "Peppy")) { isPresented = false }
        case .expired:
            Text("peppy.owner_phone_expired_headline", tableName: "Peppy").font(.title2.bold())
            Text("peppy.owner_phone_expired_body", tableName: "Peppy")
            Button(String(localized: "peppy.owner_phone_restart_cta", table: "Peppy")) { Task { await start() } }
                .accessibilityIdentifier("owner-phone-restart")
        case .denied:
            Text("peppy.owner_phone_error_denied", tableName: "Peppy").foregroundStyle(.red)
                .accessibilityIdentifier("owner-phone-denied")
            Button(String(localized: "peppy.owner_phone_restart_cta", table: "Peppy")) { Task { await start() } }
                .accessibilityIdentifier("owner-phone-restart")
        case .none:
            connectingSpinner
        }
    }

    private var connectingSpinner: some View {
        ProgressView().accessibilityLabel(Text("peppy.production_pairing_connecting", tableName: "Peppy"))
    }

    private var countdownText: String {
        let seconds = Duration.seconds(status?.expiresInSeconds ?? 0)
        let remaining = seconds.formatted(.units(allowed: [.seconds], width: .wide))
        return "\(String(localized: "peppy.owner_phone_countdown_label", table: "Peppy")) \(remaining)"
    }

    private var approvalPrompt: some View {
        NavigationStack {
            Form {
                Section {
                    Text(String(localized: "peppy.owner_phone_prompt_body", table: "Peppy")).font(.footnote)
                    SecureField(String(localized: "peppy.passphrase_field", table: "Peppy"), text: $promptSecret)
                        .textContentType(.password).accessibilityIdentifier("owner-phone-prompt-field")
                } header: { Text("peppy.owner_phone_prompt_title", tableName: "Peppy") }
            }
            .formStyle(.grouped)
            .navigationTitle(Text("peppy.owner_phone_prompt_title", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(String(localized: "peppy.cancel", table: "Peppy")) { resumePrompt(with: nil) }
                        .accessibilityIdentifier("owner-phone-prompt-cancel")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(String(localized: "peppy.continue", table: "Peppy")) { resumePrompt(with: promptSecret) }
                        .disabled(promptSecret.isEmpty).accessibilityIdentifier("owner-phone-prompt-confirm")
                }
            }
        }
        .accessibilityIdentifier("owner-phone-approval-prompt")
        .interactiveDismissDisabled()
    }

    private func start() async {
        apply(await host.start(method: requestedMethod))
    }

    /// Waiting rotates the invitation when needed. Verify and Approved only refresh status, so a stale
    /// poll cannot hide an approval. Polling stops at Connected, Expired, and Denied.
    private func pollUntilSettled() async {
        while !Task.isCancelled, let stage = status?.stage, stage == .waiting || stage == .verify || stage == .approved {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !isBusy else { continue }
            if stage == .waiting {
                isRotating = true
                apply(await host.rotateIfNeeded(requested: requestedMethod))
                isRotating = false
            } else {
                apply(await host.poll(), keepsError: true)
            }
        }
    }

    /// The reveal is offered only after the invitation has switched to the shared-secret method, as on desktop.
    private func revealSharedSecret() async {
        requestedMethod = .sharedSecret
        apply(await host.rotateIfNeeded(requested: .sharedSecret))
        guard errorCode == nil else { return }
        revealedSecret = await revealSecret()
    }

    private func approve() async {
        isBusy = true
        apply(await host.approve(prompt: promptForSecret))
        isBusy = false
    }

    /// Cancel always closes the sheet. The Waiting invitation is cancelled best-effort.
    private func cancel() async {
        resumePrompt(with: nil)
        isPresented = false
        _ = await host.cancel()
    }

    private func retry() async {
        errorCode = nil
        if status?.stage == .verify { await approve() } else { await start() }
    }

    private func apply(_ result: OwnerPhoneResult, keepsError: Bool = false) {
        switch result {
        case .view(let next, let nextQR):
            status = next
            qr = nextQR ?? qr
            if !keepsError { errorCode = nil }
            if next.stage != .verify { codesMatch = false }
        case .cancelled: isPresented = false
        case .failed(let code): errorCode = code
        }
    }

    private func promptForSecret() async -> String? {
        await withCheckedContinuation { continuation in
            promptContinuation = continuation
            showingApprovalPrompt = true
        }
    }

    private func resumePrompt(with value: String?) {
        let continuation = promptContinuation
        promptContinuation = nil
        let result = value
        promptSecret = ""
        showingApprovalPrompt = false
        continuation?.resume(returning: result)
    }

    private func qrImage(_ value: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(value.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 10, y: 10)),
              let image = CIContext().createCGImage(output, from: output.extent)
        else { return nil }
        return UIImage(cgImage: image)
    }

    private func errorMessage(_ code: String) -> String {
        let key = switch code {
        case "wrong-passphrase": "peppy.owner_phone_error_wrong_passphrase"
        case "vault-secret-mismatch": "peppy.owner_phone_error_secret_mismatch"
        case "vault-secret-unavailable": "peppy.owner_phone_error_secret_unavailable"
        case "secure-store-unavailable": "peppy.production_secure_storage_failed"
        case "offline": "peppy.production_network_failed"
        case "expired": "peppy.owner_phone_expired_body"
        case "enrollment-state": "peppy.owner_phone_error_state"
        default: "peppy.owner_phone_error_server_rejected"
        }
        return NSLocalizedString(key, tableName: "Peppy", comment: "")
    }
}
