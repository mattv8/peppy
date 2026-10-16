import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit
#if canImport(PeppyNative)
import PeppyNative
#endif

@MainActor
struct OwnerPhoneApprovalView: View {
    private enum Operation: Equatable {
        case none, preparing, refreshing, approving, revealing, cancelling
    }

    let host: OwnerPhoneApprovalHost
    @Binding var isPresented: Bool
    /// The Phase-B owner supplies a device-authenticated, transient reveal surface.
    let revealSecret: () async -> AppModel.OwnerVaultSecretRevealOutcome

    @State private var status: OwnerPhoneStatus?
    @State private var qr: String?
    @State private var codesMatch = false
    @State private var errorCode: String?
    @State private var operation = Operation.none
    @State private var showingApprovalPrompt = false
    @State private var promptSecret = ""
    @State private var promptContinuation: CheckedContinuation<String?, Never>?
    @State private var revealedSecret: String?
    @State private var revealError: String?
    @State private var showingRevealSheet = false
    @State private var requestedMethod = OwnerPhoneMethod.transfer
    @State private var hasCancelledInvitation = false
    @State private var revealOperationID: UUID?
    @State private var polling = OwnerPhonePollingCoordinator()
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    init(host: OwnerPhoneApprovalHost, isPresented: Binding<Bool>, revealSecret: @escaping () async -> AppModel.OwnerVaultSecretRevealOutcome = { .unavailable }) {
        self.host = host
        _isPresented = isPresented
        self.revealSecret = revealSecret
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    stageContent
                    if let label = operationLabel {
                        operationIndicator(label, id: "owner-phone-operation")
                    }
                    if let errorCode {
                        Text(errorMessage(errorCode))
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("owner-phone-error")
                        Button(String(localized: "peppy.try_again", table: "Peppy")) { Task { await retry() } }
                            .disabled(isBusy)
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
                        .disabled(isBusy)
                        .accessibilityIdentifier("owner-phone-cancel")
                }
            }
        }
        .accessibilityIdentifier("owner-phone-screen")
        .task { await start() }
        .task(id: status?.stage) { await pollUntilSettled() }
        .sheet(isPresented: $showingApprovalPrompt) { approvalPrompt }
        .sheet(isPresented: $showingRevealSheet, onDismiss: { revealedSecret = nil; revealError = nil }) {
            NavigationStack {
                Group {
                    if let revealedSecret {
                        Text(revealedSecret)
                            .font(.body.monospaced())
                            .textSelection(.enabled)
                            .privacySensitive()
                            .accessibilityIdentifier("owner-phone-reveal")
                    } else if let revealError {
                        Text(revealError)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("owner-phone-reveal-error")
                    }
                }
                .padding(24)
                .navigationTitle(Text("peppy.owner_phone_secret_reveal_cta", tableName: "Peppy"))
                .toolbar { ToolbarItem(placement: .topBarTrailing) { Button(String(localized: "peppy.owner_phone_secret_hide_cta", table: "Peppy")) { showingRevealSheet = false } } }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            // The revealed secret must not stay on screen while the app is inactive or in the app switcher.
            if phase != .active {
                revealedSecret = nil
                showingRevealSheet = false
                revealOperationID = nil
            }
        }
        .onDisappear {
            resumePrompt(with: nil)
            revealedSecret = nil
            revealError = nil
            revealOperationID = nil
            cancelWaitingInvitation()
        }
    }

    @ViewBuilder private var stageContent: some View {
        switch status?.stage {
        case .waiting:
            Text("peppy.owner_phone_waiting_headline", tableName: "Peppy").font(.title2.bold())
            Text("peppy.owner_phone_waiting_body", tableName: "Peppy")
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
            .disabled(isBusy)
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
        case .approved:
            // The approved prose doubles as the persistent waiting indicator, so it is announced once.
            operationIndicator("peppy.owner_phone_approved_body", id: "owner-phone-approved")
                .font(.title2.bold())
        case .connected:
            Text("peppy.owner_phone_connected_headline", tableName: "Peppy").font(.title2.bold())
                .accessibilityAddTraits(.isHeader).accessibilityIdentifier("owner-phone-connected")
            Text("peppy.owner_phone_connected_body", tableName: "Peppy")
            Button(String(localized: "peppy.owner_phone_done_cta", table: "Peppy")) { isPresented = false }
        case .expired:
            Text("peppy.owner_phone_expired_headline", tableName: "Peppy").font(.title2.bold())
            Text("peppy.owner_phone_expired_body", tableName: "Peppy")
            Button(String(localized: "peppy.owner_phone_restart_cta", table: "Peppy")) { Task { await start() } }
                .disabled(isBusy)
                .accessibilityIdentifier("owner-phone-restart")
        case .denied:
            Text("peppy.owner_phone_error_denied", tableName: "Peppy").foregroundStyle(.red)
                .accessibilityIdentifier("owner-phone-denied")
            Button(String(localized: "peppy.owner_phone_restart_cta", table: "Peppy")) { Task { await start() } }
                .disabled(isBusy)
                .accessibilityIdentifier("owner-phone-restart")
        case .none:
            EmptyView()
        }
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
        guard !isBusy else { return }
        let action = polling.beginAction()
        operation = .preparing
        defer { polling.finishAction(action); operation = .none }
        apply(await host.start(method: requestedMethod))
    }

    /// Waiting rotates the invitation when needed. Verify and Approved only refresh status, so a stale
    /// poll cannot hide an approval. Polling stops at Connected, Expired, and Denied.
    private func pollUntilSettled() async {
        while !Task.isCancelled, let stage = status?.stage, stage == .waiting || stage == .verify || stage == .approved {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !isBusy else { continue }
            if stage == .waiting {
                let poll = polling.beginPoll()
                let showsRefresh = (status?.expiresInSeconds ?? 0) <= 15
                if showsRefresh { operation = .refreshing }
                let result = await host.rotateIfNeeded(requested: requestedMethod)
                guard !Task.isCancelled, isPresented, polling.shouldApplyPoll(poll) || showsRefresh else { continue }
                apply(result)
                if showsRefresh { operation = .none }
            } else {
                let poll = polling.beginPoll()
                let result = await host.poll()
                guard !Task.isCancelled, isPresented, polling.shouldApplyPoll(poll) else { continue }
                apply(result, keepsError: true)
            }
        }
    }

    /// The reveal is offered only after the invitation has switched to the shared-secret method, as on desktop.
    private func revealSharedSecret() async {
        guard !isBusy else { return }
        let action = polling.beginAction()
        operation = .refreshing
        defer { polling.finishAction(action); operation = .none }
        let rotation = await host.rotateIfNeeded(requested: .sharedSecret)
        guard case let .view(next, _) = rotation, next.stage == .waiting else {
            apply(rotation)
            return
        }
        requestedMethod = .sharedSecret
        apply(rotation)
        operation = .revealing
        let operationID = UUID()
        revealOperationID = operationID
        defer { if revealOperationID == operationID { revealOperationID = nil } }
        switch await revealSecret() {
        case .revealed(let secret):
            guard isPresented, scenePhase == .active, revealOperationID == operationID else { return }
            revealedSecret = secret
            revealError = nil
            showingRevealSheet = true
        case .cancelled:
            break
        case .unavailable:
            revealError = String(localized: "peppy.owner_phone_error_lock_required", table: "Peppy")
            showingRevealSheet = true
        case .failed:
            revealError = String(localized: "peppy.owner_phone_error_secret_unavailable", table: "Peppy")
            showingRevealSheet = true
        }
        operation = .none
    }

    private func approve() async {
        guard !isBusy else { return }
        let action = polling.beginAction()
        operation = .approving
        defer { polling.finishAction(action); operation = .none }
        apply(await host.approve(prompt: promptForSecret))
    }

    /// Only a waiting invitation can be revoked; later stages simply close this local surface.
    private func cancel() async {
        guard !isBusy else { return }
        resumePrompt(with: nil)
        guard status?.stage == .waiting else {
            isPresented = false
            return
        }
        hasCancelledInvitation = true
        let action = polling.beginAction()
        operation = .cancelling
        let result = await host.cancel()
        polling.finishAction(action)
        operation = .none
        switch result {
        case .cancelled: isPresented = false
        case .view, .failed:
            hasCancelledInvitation = false
            apply(result)
        }
    }

    private func retry() async {
        // A 404/410 poll leaves the stage at Waiting; that invitation is gone, so rotating it would fail again.
        let invitationGone = errorCode == "expired"
        errorCode = nil
        switch status?.stage {
        case .verify:
            await approve()
        case .approved:
            apply(await host.poll(), keepsError: true)
        case .waiting where invitationGone:
            await start()
        case .waiting:
            apply(await host.rotateIfNeeded(requested: requestedMethod), keepsError: true)
        case .expired, .denied, .none:
            await start()
        case .connected:
            break
        }
    }

    private func cancelWaitingInvitation() {
        guard status?.stage == .waiting, !hasCancelledInvitation, !isBusy else { return }
        hasCancelledInvitation = true
        Task { _ = await host.cancel() }
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

    private var isBusy: Bool { operation != .none }

    private var operationLabel: String? {
        switch operation {
        case .none: nil
        case .preparing: "peppy.production_pairing_preparing"
        case .refreshing: "peppy.production_pairing_refreshing"
        case .approving: "peppy.production_pairing_approving"
        case .revealing: "peppy.production_pairing_revealing"
        case .cancelling: "peppy.production_pairing_cancelling"
        }
    }

    private func operationIndicator(_ key: String, id: String) -> some View {
        HStack {
            if reduceMotion {
                Image(systemName: "hourglass")
                    .accessibilityHidden(true)
            } else {
                ProgressView().accessibilityHidden(true)
            }
            Text(NSLocalizedString(key, tableName: "Peppy", comment: ""))
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(NSLocalizedString(key, tableName: "Peppy", comment: "")))
        .accessibilityIdentifier(id)
    }
}
