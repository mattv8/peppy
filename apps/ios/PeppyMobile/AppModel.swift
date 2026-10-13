import Foundation
import Observation
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif
#if canImport(UIKit)
import UIKit
#endif
#if canImport(PeppyNative)
import PeppyNative
#endif

/// Main-actor UI state over the `NativeSession` actor. Holds no secrets: the passphrase lives
/// only in the SecureField binding until it is handed to the core once.
@MainActor @Observable
final class AppModel {
    enum Activity: Equatable { case idle, importing, unlocking, enrolling, syncing, refreshing, disconnecting }

    private(set) var status = SessionStatus()
    private(set) var activity = Activity.idle
    private(set) var lastError: String?
    private(set) var lastSync: SyncReport?
    private(set) var lastSyncDate: Date?
    private(set) var devices: [DeviceRosterItem] = []
    private(set) var devicesLoaded = false
    private(set) var relay: RelayWakeClient?
    var relayOrigin = UserDefaults.standard.string(forKey: "peppy.relay-origin") ?? ""
    /// The active enrollment cannot be opened (e.g. its database is gone after a reinstall);
    /// disconnecting is the way out.
    private(set) var enrollmentBlocked = false
    let telephony = TelephonyEligibility.evaluate()
    /// Contact sync settings and last pass, shown by `ContactsSection`.
    var contacts = ContactsState()

    let session: NativeSession
    let contactsPreferences = UserDefaultsContactsPreferences()
    let contactsProvider: any ContactsProvider = CNContactStoreProvider()
    /// One contact pass at a time across foreground, change notifications and BackgroundTasks.
    @ObservationIgnored private(set) var contactPasses: ContactsPassCoordinator!
    #if os(iOS)
    /// Retained for the process lifetime; BackgroundTasks hold no other reference.
    @ObservationIgnored private var contactsScheduler: ContactsBackgroundScheduler?
    #endif
    @ObservationIgnored var contactsObserver: (any NSObjectProtocol)?

    init() {
        #if DEBUG
        let allowLoopbackHTTP = true
        #else
        let allowLoopbackHTTP = false
        #endif
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        session = NativeSession(
            secureStore: KeychainSecureStore(),
            transport: URLSessionTransport(),
            databaseDirectory: support.appendingPathComponent("Peppy", isDirectory: true),
            allowLoopbackHTTP: allowLoopbackHTTP
        )
        let session = self.session
        let provider = contactsProvider
        let preferences = contactsPreferences
        contactPasses = ContactsPassCoordinator { [weak self] reason, isCancelled in
            var budget = SyncBudget()
            if reason == .backgroundRefresh {
                // BGAppRefresh grants about 25 seconds; keep the network share small.
                budget.maxRequests = 8
                budget.maxUploads = 4
                budget.maxApplyRounds = 4
            }
            let report = try await session.contactsPass(
                reason: reason, provider: provider, preferences: preferences, budget: budget, isCancelled: isCancelled
            )
            await self?.contactsPassFinished(report)
            return [.completed, .unchanged, .incremental].contains(report.outcome)
        }
        contacts.enabled = preferences.load().enabled
        restoreRelayIfOptedIn()
    }

    /// Must run during launch, before the app finishes launching (BackgroundTasks requirement).
    func registerBackgroundTasks() {
        #if os(iOS)
        let scheduler = ContactsBackgroundScheduler(coordinator: contactPasses)
        scheduler.register()
        contactsScheduler = scheduler
        if contacts.enabled { scheduler.scheduleAll() }
        #endif
    }

    func scheduleContactsBackgroundWork() {
        #if os(iOS)
        contactsScheduler?.scheduleAll()
        #endif
    }

    func load() async {
        await run(.idle) { try await $0.open() }
    }

    func importCredential(from result: Result<URL, any Error>) async {
        guard case .success(let url) = result else {
            lastError = "The credential file could not be selected."
            return
        }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        await run(.importing) { try await $0.importCredential(file: url) }
    }

    func refresh() async {
        await run(.refreshing) { try await $0.refreshEnrollment() }
    }

    /// Non-destructive: the encrypted database, keys and queued work stay archived on this device.
    func disconnect() async {
        await run(.disconnecting) { try await $0.disconnect() }
        if status.identity == nil {
            await relay?.revoke()
            relay = nil
            UserDefaults.standard.set(false, forKey: "peppy.relay-opted-in")
        }
        lastSync = nil
        lastSyncDate = nil
    }

    func disconnectLocalOnly() async {
        await run(.disconnecting) { try await $0.disconnectLocalOnly() }
        if status.identity == nil {
            await relay?.revoke()
            relay = nil
            UserDefaults.standard.set(false, forKey: "peppy.relay-opted-in")
            lastError = "Signed out locally only. This device credential may remain active on the server until it can be revoked."
        }
    }

    func unlock(passphrase: String) async {
        await run(.unlocking) { try await $0.unlock(passphrase: passphrase) }
    }

    func claimInstance(origin: String, setupToken: String, username: String, adminPassword: String, passphrase: String) async {
        activity = .enrolling
        defer { activity = .idle }
        do {
            status = try await session.claimInstance(
                origin: origin,
                setupToken: setupToken,
                username: username,
                adminPassword: adminPassword,
                passphrase: passphrase,
                deviceName: UIDevice.current.name
            )
            lastError = nil
            enrollmentBlocked = false
        } catch {
            let failure = error as? ClientError
            lastError = setupErrorMessage(for: failure) ?? failure?.userMessage ?? "Unexpected error."
        }
    }

    func discoverSelfHostedInstance(origin: String) async -> SelfHostedInstanceDiscovery? {
        activity = .enrolling
        defer { activity = .idle }
        do {
            let discovery = try await session.discoverSelfHostedInstance(origin: origin)
            lastError = nil
            return discovery
        } catch {
            lastError = setupErrorMessage(for: error as? ClientError) ?? "Could not check this server. Verify the URL and try again."
            return nil
        }
    }

    func joinWithSharedSecret(origin: String, vaultID: String?, vaultOwner: String?, passphrase: String) async {
        await run(.enrolling) {
            try await $0.joinWithSharedSecret(
                origin: origin,
                vaultID: vaultID,
                vaultOwner: vaultOwner,
                passphrase: passphrase,
                deviceName: UIDevice.current.name
            )
        }
    }

    func signIn(origin: String, username: String, password: String) async -> Bool {
        activity = .enrolling
        defer { activity = .idle }
        do {
            try await session.signIn(origin: origin, username: username, password: password)
            lastError = nil
            return true
        } catch {
            lastError = (error as? ClientError)?.userMessage ?? "Could not sign in to this server."
            return false
        }
    }

    func beginVaultRequest(inviteLink: String, displayName: String, expectedOrigin: String?) async -> VaultRequestProgress? {
        activity = .enrolling
        defer { activity = .idle }
        do {
            let progress = try await session.beginVaultRequest(inviteLink: inviteLink, displayName: displayName, expectedOrigin: expectedOrigin, deviceName: UIDevice.current.name)
            lastError = nil
            return progress
        } catch {
            lastError = (error as? ClientError)?.userMessage ?? "Could not send the vault request."
            return nil
        }
    }

    func pendingVaultRequest(origin: String) async -> VaultRequestProgress? {
        do { return try await session.pendingVaultRequest(origin: origin) }
        catch { lastError = (error as? ClientError)?.userMessage; return nil }
    }

    func vaultRequestStatus(origin: String) async -> VaultRequestProgress? {
        do { return try await session.vaultRequestStatus(origin: origin) }
        catch is CancellationError { return nil }
        catch ClientError.canceled { return nil }
        catch { lastError = (error as? ClientError)?.userMessage ?? "Could not check the vault request."; return nil }
    }

    func createVault(origin: String, username: String, adminPassword: String, passphrase: String) async {
        await run(.enrolling) {
            try await $0.createVault(origin: origin, username: username, adminPassword: adminPassword, passphrase: passphrase)
        }
    }

    func cancelVaultRequest(origin: String) {
        Task { try? await session.cancelVaultRequest(origin: origin) }
    }

    func vaultRequestRecoveryRequired() {
        lastError = "This vault request completed, but its local recovery material is unavailable. Ask the server admin for a new invite."
    }

    func vaultRequestRecoveryRetained() {
        lastError = "Your prepared vault request is kept securely on this device. Return with the shared secret you saved to finish creating the vault."
    }

    func beginPhoneEnrollment(qr: String) async -> PhoneEnrollmentStatus? {
        lastError = nil
        activity = .enrolling
        defer { activity = .idle }
        do { return try await session.beginPhoneEnrollment(qrPayload: qr, deviceName: UIDevice.current.name) }
        catch { lastError = (error as? ClientError)?.userMessage ?? "Could not scan this enrollment code."; return nil }
    }

    func isVaultInviteLink(_ value: String) async -> Bool {
        (try? await session.vaultInviteOrigin(link: value)) != nil
    }

    func phoneEnrollmentStatus() async -> PhoneEnrollmentStatus? {
        do { return try await session.phoneEnrollmentStatus() }
        catch { lastError = (error as? ClientError)?.userMessage ?? "Pairing expired or was denied."; return nil }
    }

    func confirmPhoneEnrollment(sharedSecret: String?) async -> PhoneEnrollmentAttemptCoordinator.ConfirmationOutcome {
        lastError = nil
        activity = .enrolling
        defer { activity = .idle }
        do {
            status = try await session.confirmPhoneEnrollment(sharedSecret: sharedSecret)
            lastError = nil
            enrollmentBlocked = false
            return .succeeded
        } catch is CancellationError {
            return .canceled
        } catch ClientError.canceled {
            return .canceled
        } catch {
            let failure = error as? ClientError
            lastError = failure?.userMessage ?? "Unexpected error."
            switch failure {
            case .localDatabaseMissing, .missingSecret, .identityMismatch: enrollmentBlocked = true
            default: break
            }
            let retainedRequest = await session.phoneEnrollmentCanRetryConfirmation()
            if retainedRequest, case .some(.network) = failure { return .retryableFailure }
            if retainedRequest, case let .some(.server(status, _)) = failure, status >= 500 { return .retryableFailure }
            return .terminalFailure
        }
    }

    func cancelPhoneEnrollment() { Task { await session.cancelPhoneEnrollment() } }

    func ownerPhoneApprovalHost() async -> OwnerPhoneApprovalHost? {
        do {
            return try await session.ownerPhoneApprovalHost(deviceName: UIDevice.current.name)
        } catch {
            lastError = (error as? ClientError)?.userMessage ?? "Could not prepare phone approval."
            return nil
        }
    }

    func revealOwnerVaultSecret() async -> String? {
        #if canImport(LocalAuthentication)
        let context = LAContext()
        do {
            guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else {
                lastError = String(localized: "peppy.owner_phone_error_lock_required", table: "Peppy")
                return nil
            }
            try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: String(localized: "peppy.owner_phone_secret_reveal_cta", table: "Peppy"))
            return try await session.ownerVaultSecret()
        } catch {
            lastError = String(localized: "peppy.owner_phone_error_secret_unavailable", table: "Peppy")
            return nil
        }
        #else
        lastError = String(localized: "peppy.owner_phone_error_lock_required", table: "Peppy")
        return nil
        #endif
    }

    func refreshDevices() async {
        defer { devicesLoaded = true }
        do { devices = try await session.devices() }
        catch { lastError = (error as? ClientError)?.userMessage ?? "Could not load devices." }
    }

    func revokeDevice(_ id: String) async {
        await run(.disconnecting) { try await $0.revokeDevice(id) }
        await refreshDevices()
    }

    func deleteVault() async {
        guard let vault = status.identity?.vaultId else { return }
        activity = .disconnecting
        defer { activity = .idle }
        do { try await session.deleteVault(vaultId: vault); status = SessionStatus(); devices = []; lastError = nil }
        catch { lastError = (error as? ClientError)?.userMessage ?? "Could not delete vault." }
    }

    func configureRelay() {
        do {
            let origin = try ServerOrigin(canonical: relayOrigin, allowLoopbackHTTP: false)
            relay = RelayWakeClient(
                origin: origin, transport: URLSessionTransport(), secure: KeychainSecureStore(),
                publisher: SessionRelayWakeRoutePublisher(session: session)
            )
            UserDefaults.standard.set(origin.serialized, forKey: "peppy.relay-origin")
            UserDefaults.standard.set(true, forKey: "peppy.relay-opted-in")
            relayOrigin = origin.serialized
            lastError = nil
            #if canImport(UIKit)
            UIApplication.shared.registerForRemoteNotifications()
            #endif
        } catch { lastError = "Enter a canonical HTTPS relay origin." }
    }

    func wakeSync() async -> Bool {
        // A background APNs launch has no SwiftUI `.task` to open the session first.
        guard let opened = try? await session.open(), opened.databaseOpen, opened.keysUnlocked else { return false }
        do { _ = try await session.syncOnce(); return true } catch { return false }
    }

    func wakeContacts() async -> Bool {
        guard contacts.enabled else { return true }
        await runContactsPass(.backgroundRefresh)
        return !Task.isCancelled
    }

    private func restoreRelayIfOptedIn() {
        guard UserDefaults.standard.bool(forKey: "peppy.relay-opted-in"), !relayOrigin.isEmpty else { return }
        configureRelay()
    }

    /// One bounded pass while the app is in the foreground. SwiftUI cancels the calling task when
    /// the scene leaves the foreground; there is no background or long-lived connection.
    func syncInForeground() async {
        guard status.databaseOpen, status.keysUnlocked, activity == .idle else { return }
        activity = .syncing
        defer { activity = .idle }
        do {
            let report = try await session.syncOnce()
            lastSync = report
            lastSyncDate = Date()
            lastError = report.uploadRefused.map { "Upload refused; the message stays queued. \($0.userMessage)" }
        } catch ClientError.canceled {
            // Leaving the foreground is not a failure.
        } catch ClientError.syncInProgress {
            // A contact pass holds the sync guard; it uploads and receives too.
        } catch is CancellationError {
        } catch {
            lastError = (error as? ClientError)?.userMessage ?? "Sync failed."
        }
        status = (try? await session.open()) ?? status
        await runContactsPass(.foreground)
    }

    private func run(_ activity: Activity, _ body: (NativeSession) async throws -> SessionStatus) async {
        self.activity = activity
        defer { self.activity = .idle }
        do {
            status = try await body(session)
            lastError = nil
            enrollmentBlocked = false
        } catch {
            let failure = error as? ClientError
            lastError = failure?.userMessage ?? "Unexpected error."
            switch failure {
            case .localDatabaseMissing, .missingSecret, .identityMismatch: enrollmentBlocked = true
            default: break
            }
        }
    }

    private func setupErrorMessage(for error: ClientError?) -> String? {
        switch error {
        case .server(_, "invalid_setup_token"):
            String(localized: "peppy.enrollment_error_invalid_setup_token", table: "Peppy")
        case .server(_, "instance_already_claimed"):
            String(localized: "peppy.enrollment_error_already_claimed", table: "Peppy")
        default:
            nil
        }
    }
}
