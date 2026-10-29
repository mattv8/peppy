import AVFoundation
import SwiftUI
#if canImport(UIKit)
import UIKit
#endif
#if canImport(PeppyNative)
import PeppyNative
#endif

@MainActor
struct OwnerComputerApprovalView: View {
    private enum CameraState { case ready, scanning, denied }

    let host: OwnerComputerApprovalHost
    @Binding var isPresented: Bool
    @State private var camera = CameraState.ready
    @State private var approval: OwnerComputerApproval?
    @State private var codesMatch = false
    @State private var errorCode: String?
    @State private var busy = false
    @State private var approved = false
    @State private var showingPhrasePrompt = false
    @State private var phrase = ""
    @State private var phraseContinuation: CheckedContinuation<String?, Never>?
    @Environment(\.colorScheme) private var colorScheme

    private var colors: PeppyColorScheme { PeppyTokens.colors(for: colorScheme) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    content
                    if busy { ProgressView().accessibilityIdentifier("owner-computer-progress") }
                    if let errorCode {
                        Text(message(for: errorCode)).foregroundStyle(colors.Error).accessibilityIdentifier("owner-computer-error")
                    }
                }
                .padding(24)
                .frame(maxWidth: 460)
            }
            .navigationTitle(Text("peppy.owner_computer_nav_title", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if approved {
                        Button(String(localized: "peppy.owner_computer_done_cta", table: "Peppy")) { isPresented = false }
                            .accessibilityIdentifier("owner-computer-done")
                    } else {
                        Button(String(localized: "peppy.cancel", table: "Peppy")) { leaveFlow() }
                            .accessibilityIdentifier("owner-computer-cancel")
                    }
                }
            }
        }
        .interactiveDismissDisabled(!approved)
        .accessibilityIdentifier("owner-computer-screen")
        .sheet(isPresented: $showingPhrasePrompt) { phrasePrompt }
        .onChange(of: errorCode) { _, errorCode in
            guard let errorCode else { return }
            #if canImport(UIKit)
            UIAccessibility.post(notification: .announcement, argument: message(for: errorCode))
            #endif
        }
        .onDisappear {
            phrase = ""
            phraseContinuation?.resume(returning: nil)
            phraseContinuation = nil
            Task { await host.dismiss() }
        }
    }

    @ViewBuilder private var content: some View {
        if approved {
            Text(String(localized: "peppy.owner_computer_approved_title", table: "Peppy")).font(.title2.bold())
                .accessibilityIdentifier("owner-computer-approved")
            Text(String(localized: "peppy.owner_computer_approved_body", table: "Peppy"))
        } else if let approval {
            Text(String(localized: "peppy.owner_computer_verify_body", table: "Peppy")).font(.title2.bold())
            Text(approval.sas).font(.title.monospaced())
                .accessibilityIdentifier("owner-computer-sas")
            Toggle(String(localized: "peppy.owner_computer_codes_match", table: "Peppy"), isOn: $codesMatch)
                .frame(minHeight: 44)
                .accessibilityIdentifier("owner-computer-codes-match")
            Button(String(localized: "peppy.owner_computer_approve_cta", table: "Peppy")) { Task { await approve() } }
                .buttonStyle(.borderedProminent)
                .disabled(!codesMatch || busy)
                .accessibilityIdentifier("owner-computer-approve")
        } else {
            Text(String(localized: "peppy.owner_computer_scan_intro", table: "Peppy"))
            switch camera {
            case .ready:
                Button(String(localized: "peppy.owner_computer_scan_cta", table: "Peppy")) { requestCamera() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy)
                    .accessibilityIdentifier("owner-computer-scan")
            case .scanning:
                QRScannerView { code in Task { await scan(code) } }
                    .frame(height: 300)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    .accessibilityIdentifier("owner-computer-scanner")
                Button(String(localized: "peppy.back", table: "Peppy")) { camera = .ready }
                    .disabled(busy)
                    .accessibilityIdentifier("owner-computer-scan-back")
            case .denied:
                Text(String(localized: "peppy.owner_computer_camera_denied", table: "Peppy"))
                    .accessibilityIdentifier("owner-computer-camera-denied")
                Button(String(localized: "peppy.owner_computer_open_settings_cta", table: "Peppy")) { openSettings() }
                    .accessibilityIdentifier("owner-computer-open-settings")
                Button(String(localized: "peppy.back", table: "Peppy")) { camera = .ready }
                    .accessibilityIdentifier("owner-computer-camera-back")
            }
        }
    }

    private var phrasePrompt: some View {
        NavigationStack {
            Form {
                Section(String(localized: "peppy.owner_computer_phrase_title", table: "Peppy")) {
                    Text(String(localized: "peppy.owner_computer_phrase_body", table: "Peppy")).font(.footnote)
                    SecureField(String(localized: "peppy.passphrase_field", table: "Peppy"), text: $phrase)
                        .textContentType(.password)
                        .accessibilityIdentifier("owner-computer-phrase-field")
                }
            }
            .navigationTitle(Text("peppy.owner_computer_phrase_title", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(String(localized: "peppy.cancel", table: "Peppy")) { resumePhrase(nil) }.accessibilityIdentifier("owner-computer-phrase-cancel")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(String(localized: "peppy.continue", table: "Peppy")) { resumePhrase(phrase) }
                        .disabled(phrase.isEmpty)
                        .accessibilityIdentifier("owner-computer-phrase-confirm")
                }
            }
        }
        .interactiveDismissDisabled()
        .accessibilityIdentifier("owner-computer-phrase-prompt")
    }

    private func requestCamera() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: camera = .scanning
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                Task { @MainActor in camera = granted ? .scanning : .denied }
            }
        case .denied, .restricted: camera = .denied
        @unknown default: camera = .denied
        }
    }

    private func scan(_ code: String) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        switch await host.scan(qr: code) {
        case let .view(value):
            approval = value
            camera = .ready
            errorCode = nil
        case let .failed(error):
            errorCode = error
            camera = .ready
        case .cancelled, .approved:
            errorCode = "enrollment-state"
            camera = .ready
        }
    }

    private func approve() async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        switch await host.approve(codesMatch: codesMatch, prompt: promptForPhrase) {
        case .approved:
            approved = true
            errorCode = nil
        case .cancelled:
            errorCode = nil
        case let .failed(error):
            errorCode = error
        case .view:
            errorCode = "enrollment-state"
        }
    }

    private func promptForPhrase() async -> String? {
        await withCheckedContinuation { continuation in
            phrase = ""
            phraseContinuation = continuation
            showingPhrasePrompt = true
        }
    }

    private func resumePhrase(_ value: String?) {
        let continuation = phraseContinuation
        phraseContinuation = nil
        showingPhrasePrompt = false
        phrase = ""
        continuation?.resume(returning: value)
    }

    private func leaveFlow() {
        isPresented = false
        Task { await host.cancel() }
    }

    private func openSettings() {
        #if canImport(UIKit)
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
        #endif
    }

    private func message(for code: String) -> String {
        switch code {
        case "invalid-computer-qr": return String(localized: "peppy.owner_computer_error_invalid_qr", table: "Peppy")
        case "codes-do-not-match": return String(localized: "peppy.owner_computer_error_codes_mismatch", table: "Peppy")
        case "secure-store-unavailable": return String(localized: "peppy.owner_computer_error_secure_store", table: "Peppy")
        case "account-required": return String(localized: "peppy.owner_computer_error_account_required", table: "Peppy")
        case "offline": return String(localized: "peppy.owner_computer_error_offline", table: "Peppy")
        case "expired": return String(localized: "peppy.owner_computer_error_expired", table: "Peppy")
        case "server-rejected": return String(localized: "peppy.owner_computer_error_server_rejected", table: "Peppy")
        case "enrollment": return String(localized: "peppy.owner_computer_error_enrollment", table: "Peppy")
        default: return String(localized: "peppy.owner_computer_error_generic", table: "Peppy")
        }
    }
}
