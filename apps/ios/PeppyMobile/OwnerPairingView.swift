import SwiftUI
import CoreImage
#if canImport(PeppyNative)
import PeppyNative
#endif

@MainActor
struct OwnerPairingView: View {
    let model: AppModel
    @Binding var isPresented: Bool
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    private enum Phase { case idle, creating, waiting, claimed, approving, failed }
    @State private var phase = Phase.idle
    @State private var intent: OwnerPairingIntent?
    @State private var claim: OwnerPairingClaim?
    @State private var codesMatched = false
    @State private var errorKey: String?
    @State private var task: Task<Void, Never>?
    @State private var generation = 0
    @State private var qrImage: UIImage?
    @State private var showingAddComputer = false

    private var colors: PeppyColorScheme { PeppyTokens.colors(for: colorScheme) }
    private var busy: Bool { phase == .creating || phase == .approving }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    if let qrImage {
                        Image(uiImage: qrImage).interpolation(.none).resizable().scaledToFit()
                            .frame(maxHeight: 240)
                            .accessibilityLabel(Text("peppy.production_pairing_qr_accessibility", tableName: "Peppy"))
                            .accessibilityIdentifier("owner-pairing-qr")
                        Text("peppy.production_pairing_qr_body", tableName: "Peppy")
                            .foregroundStyle(colors.TextSecondary)
                    }
                    if let claim {
                        VStack(spacing: 12) {
                            Text("peppy.production_owner_sas_heading", tableName: "Peppy").font(.headline)
                            Text(claim.deviceId).font(.caption.monospaced()).textSelection(.enabled)
                                .accessibilityIdentifier("owner-pairing-device-id")
                            LabeledContent {
                                Text(LocalizedStringKey(claim.requestedRole == "gateway" ? "peppy.production_pairing_role_gateway" : "peppy.production_pairing_role_device"), tableName: "Peppy")
                            } label: {
                                Text("peppy.production_pairing_role_label", tableName: "Peppy")
                            }
                            Text(claim.sas).font(.title2.monospaced())
                                .accessibilityIdentifier("owner-pairing-sas")
                            Toggle(isOn: $codesMatched) {
                                Text("peppy.production_pairing_verify", tableName: "Peppy")
                            }.disabled(phase != .claimed).accessibilityIdentifier("owner-pairing-confirm-toggle")
                            Button(action: startApproval) {
                                Text("peppy.device_allow", tableName: "Peppy")
                            }
                            .buttonStyle(.borderedProminent).tint(colors.Accent).foregroundStyle(colors.AccentText)
                            .disabled(!codesMatched || phase != .claimed || scenePhase != .active || intent.map { $0.expiresAt <= Date() } != false)
                            .accessibilityIdentifier("owner-approve-button")
                        }.padding(12).background(colors.SurfacePanel).clipShape(RoundedRectangle(cornerRadius: 8))
                    } else if phase == .waiting {
                        Text("peppy.production_pairing_waiting", tableName: "Peppy")
                        ProgressView().accessibilityIdentifier("owner-pairing-progress")
                    }
                    if let errorKey {
                        Text(LocalizedStringKey("peppy.\(errorKey)"), tableName: "Peppy")
                            .foregroundStyle(colors.Error).accessibilityIdentifier("owner-pairing-error")
                    }
                    if busy { ProgressView().accessibilityIdentifier("owner-pairing-busy") }
                    if phase == .idle || phase == .failed {
                        if !model.status.keysUnlocked {
                            Text("peppy.production_pairing_unlocked", tableName: "Peppy")
                        }
                        Button(action: startCreate) {
                            Text(LocalizedStringKey(phase == .idle ? "peppy.production_add_device" : "peppy.production_pairing_retry"), tableName: "Peppy")
                        }
                        .buttonStyle(.borderedProminent).tint(colors.Accent).foregroundStyle(colors.AccentText)
                        .disabled(!model.status.keysUnlocked || model.status.role != "owner" || model.activity != .idle || scenePhase != .active)
                        .accessibilityIdentifier("owner-pairing-retry-button")
                        Button {
                            showingAddComputer = true
                        } label: {
                            Text("peppy.production_add_computer", tableName: "Peppy")
                        }
                        .buttonStyle(.bordered)
                        .disabled(!model.status.keysUnlocked || model.status.role != "owner" || model.activity != .idle || scenePhase != .active)
                        .accessibilityIdentifier("add-computer-button")
                    }
                }.padding(24)
            }
            .navigationTitle(Text("peppy.pair_device", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(role: .cancel) { stopWork(); isPresented = false } label: {
                        Text("peppy.cancel", tableName: "Peppy")
                    }.accessibilityIdentifier("owner-pairing-cancel-button")
                }
            }
        }
        .accessibilityIdentifier("owner-pairing-screen")
        .sheet(isPresented: $showingAddComputer) {
            AddComputerView(model: model, isPresented: $showingAddComputer)
        }
        .onDisappear { stopWork() }
        .onChange(of: scenePhase) { _, next in
            if next != .active, phase != .idle {
                stopWork(); phase = .failed; errorKey = "production_pairing_paused"
            } else if next == .active, errorKey == "production_pairing_paused", let intent, intent.expiresAt > Date() {
                startPolling(intent)
            }
        }
        .task(id: intent?.intentToken) {
            guard let current = intent else { return }
            do { try await Task.sleep(for: .seconds(min(900, max(0, current.expiresAt.timeIntervalSinceNow)))) }
            catch { return }
            guard !Task.isCancelled, intent?.intentToken == current.intentToken else { return }
            stopWork(); phase = .failed; errorKey = "production_pairing_expired"
        }
    }

    private func stopWork() {
        generation &+= 1
        task?.cancel(); task = nil
        codesMatched = false
    }

    private func startCreate() {
        guard !busy, scenePhase == .active else { return }
        stopWork(); intent = nil; claim = nil; qrImage = nil; errorKey = nil; phase = .creating
        let stamp = generation
        task = Task {
            defer { if generation == stamp { task = nil } }
            do {
                let created = try await model.session.createOwnerPairingIntent()
                try Task.checkCancellation()
                guard generation == stamp, scenePhase == .active else { return }
                guard let image = qr(created.qrPayload) else { throw ClientError.invalidResponse("pairing QR") }
                intent = created; qrImage = image; phase = .waiting
                await poll(created, stamp: stamp)
            } catch is CancellationError { }
            catch { if generation == stamp { phase = .failed; errorKey = "production_pairing_error" } }
        }
    }

    private func startPolling(_ current: OwnerPairingIntent) {
        stopWork(); claim = nil; errorKey = nil; phase = .waiting
        let stamp = generation
        task = Task {
            defer { if generation == stamp { task = nil } }
            await poll(current, stamp: stamp)
        }
    }

    private func poll(_ current: OwnerPairingIntent, stamp: Int) async {
        do {
            while current.expiresAt > Date() {
                try Task.checkCancellation()
                guard generation == stamp, scenePhase == .active else { return }
                let found = try await model.session.ownerPairingClaim(current)
                try Task.checkCancellation()
                guard generation == stamp else { return }
                if let found { claim = found; codesMatched = false; phase = .claimed; return }
                try await Task.sleep(for: .seconds(min(5, max(0, current.expiresAt.timeIntervalSinceNow))))
            }
            if generation == stamp { phase = .failed; errorKey = "production_pairing_expired" }
        } catch is CancellationError { }
        catch { if generation == stamp { phase = .failed; errorKey = "production_pairing_error" } }
    }

    private func startApproval() {
        guard phase == .claimed, codesMatched, scenePhase == .active, let intent, let claim else { return }
        guard intent.expiresAt > Date() else { phase = .failed; errorKey = "production_pairing_expired"; return }
        phase = .approving
        let stamp = generation
        task = Task {
            defer { if generation == stamp { task = nil } }
            do {
                try await model.session.approveOwnerPairing(intent, claim: claim, codesMatch: true)
                try Task.checkCancellation()
                guard generation == stamp else { return }
                await model.refresh()
                guard generation == stamp, !Task.isCancelled else { return }
                isPresented = false
            } catch is CancellationError { }
            catch { if generation == stamp { phase = .failed; codesMatched = false; errorKey = "production_pairing_error" } }
        }
    }

    private func qr(_ payload: Data) -> UIImage? {
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(payload, forKey: "inputMessage")
        filter.setValue("H", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 10, y: 10)),
              let image = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return UIImage(cgImage: image)
    }
}

@MainActor
private struct AddComputerView: View {
    let model: AppModel
    @Binding var isPresented: Bool
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    @State private var intent: OwnerPairingIntent?
    @State private var claim: OwnerPairingClaim?
    @State private var task: Task<Void, Never>?
    @State private var generation = 0
    @State private var scannerID = UUID()
    @State private var busy = false
    @State private var verified = false
    @State private var errorKey: String?

    private var colors: PeppyColorScheme { PeppyTokens.colors(for: colorScheme) }

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                if errorKey != nil {
                    Text(LocalizedStringKey("peppy.\(errorKey!)"), tableName: "Peppy")
                        .foregroundStyle(colors.Error)
                        .accessibilityIdentifier("add-computer-error")
                    Button {
                        cancelWork()
                        errorKey = nil
                        scannerID = UUID()
                    } label: {
                        Text("peppy.production_pairing_retry", tableName: "Peppy")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(colors.Accent)
                    .disabled(scenePhase != .active)
                } else if busy {
                    ProgressView().accessibilityLabel(Text("peppy.hosted_account_checking", tableName: "Peppy"))
                } else if let claim {
                    sasCard(claim)
                } else {
                    QRScannerView { value in offer(value) }
                        .id(scannerID)
                        .frame(height: 300)
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                        .overlay(RoundedRectangle(cornerRadius: 16).stroke(colors.Accent, lineWidth: 2))
                        .accessibilityLabel(Text("peppy.production_add_computer_scan_body", tableName: "Peppy"))
                        .accessibilityIdentifier("add-computer-scanner")
                    Text("peppy.production_add_computer_scan_body", tableName: "Peppy")
                        .foregroundStyle(colors.TextSecondary)
                }
            }
            .padding(16)
            .navigationTitle(Text("peppy.production_add_computer", tableName: "Peppy"))
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(role: .cancel) { cancel() } label: { Text("peppy.cancel", tableName: "Peppy") }
                        .accessibilityIdentifier("add-computer-cancel")
                }
            }
        }
        .accessibilityIdentifier("add-computer-sheet")
        .onDisappear { cancelWork() }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active {
                cancelWork()
                errorKey = "production_pairing_paused"
            }
        }
        .task(id: intent?.intentToken) {
            guard let current = intent else { return }
            do { try await Task.sleep(for: .seconds(min(900, max(0, current.expiresAt.timeIntervalSinceNow)))) }
            catch { return }
            guard !Task.isCancelled, intent?.intentToken == current.intentToken else { return }
            cancelWork()
            errorKey = "production_pairing_expired"
        }
    }

    @ViewBuilder private func sasCard(_ claim: OwnerPairingClaim) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("peppy.production_owner_sas_heading", tableName: "Peppy").font(.headline)
            LabeledContent {
                Text("peppy.production_pairing_role_computer", tableName: "Peppy")
            } label: {
                Text("peppy.production_pairing_role_label", tableName: "Peppy")
            }
            Text(claim.sas).font(.title2.monospaced())
                .frame(maxWidth: .infinity, alignment: .center)
                .accessibilityIdentifier("add-computer-sas-code")
            Toggle(isOn: $verified) {
                Text("peppy.production_pairing_verify", tableName: "Peppy")
            }.disabled(busy || scenePhase != .active).accessibilityIdentifier("add-computer-sas-confirm")
            Button { approve() } label: { Text("peppy.device_allow", tableName: "Peppy") }
                .buttonStyle(.borderedProminent).tint(colors.Accent).foregroundStyle(colors.AccentText)
                .disabled(!verified || busy || errorKey != nil || scenePhase != .active || intent.map { $0.expiresAt <= Date() } != false)
                .accessibilityIdentifier("add-computer-allow-button")
            Button(role: .cancel) { cancel() } label: { Text("peppy.device_deny", tableName: "Peppy") }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("add-computer-deny-button")
        }
        .padding(12).background(colors.SurfacePanel).clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func offer(_ payload: String) {
        guard !busy, intent == nil, scenePhase == .active else { return }
        busy = true; errorKey = nil
        let stamp = generation
        task = Task {
            defer { if generation == stamp { busy = false; task = nil } }
            do {
                let created = try await model.session.ownerOfferJoinRequest(qrData: Data(payload.utf8))
                try Task.checkCancellation()
                guard generation == stamp, scenePhase == .active else { return }
                intent = created
                await poll(created, stamp: stamp)
            } catch is CancellationError { }
            catch { if generation == stamp { errorKey = offerErrorKey(error) } }
        }
    }

    private func poll(_ current: OwnerPairingIntent, stamp: Int) async {
        do {
            while current.expiresAt > Date() {
                try Task.checkCancellation()
                guard generation == stamp, scenePhase == .active else { return }
                let found = try await model.session.ownerPairingClaim(current)
                try Task.checkCancellation()
                guard generation == stamp, scenePhase == .active else { return }
                if let found {
                    guard found.requestedRole == "device" else { errorKey = "production_pairing_error"; return }
                    claim = found; verified = false; return
                }
                try await Task.sleep(for: .seconds(min(5, max(0, current.expiresAt.timeIntervalSinceNow))))
            }
            if generation == stamp { errorKey = "production_pairing_expired" }
        } catch is CancellationError { }
        catch { if generation == stamp { errorKey = "production_network_failed" } }
    }

    private func approve() {
        guard verified, let intent, let claim, claim.requestedRole == "device", !busy, errorKey == nil, scenePhase == .active else { return }
        guard intent.expiresAt > Date() else {
            cancelWork()
            errorKey = "production_pairing_expired"
            return
        }
        busy = true
        let stamp = generation
        task = Task {
            defer { if generation == stamp { busy = false; task = nil } }
            do {
                try await model.session.approveOwnerPairing(intent, claim: claim, codesMatch: true)
                try Task.checkCancellation()
                guard generation == stamp else { return }
                await model.refresh()
                guard generation == stamp, !Task.isCancelled else { return }
                isPresented = false
            } catch is CancellationError { }
            catch { if generation == stamp { errorKey = "production_pairing_error"; verified = false } }
        }
    }

    private func offerErrorKey(_ error: Error) -> String {
        switch error {
        case ClientError.originMismatch:
            "production_add_computer_origin_mismatch"
        case ClientError.server(status: 410, code: "join_request_expired"), ClientError.server(status: 404, code: _):
            "production_pairing_expired"
        case ClientError.server(status: 409, code: "join_request_already_offered"):
            "production_add_computer_already_linked"
        case ClientError.server(status: 409, code: "pairing_intent_not_offerable"):
            "production_pairing_error"
        default:
            "production_network_failed"
        }
    }

    private func cancelWork() {
        generation &+= 1
        task?.cancel(); task = nil
        verified = false
        busy = false
        intent = nil
        claim = nil
    }

    private func cancel() {
        cancelWork()
        isPresented = false
    }
}
