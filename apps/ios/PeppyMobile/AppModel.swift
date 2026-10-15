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
    private struct WakeEnrollmentBinding {
        let identity: RelayWakeIdentity
        let target: RelayWakeRegistrationTarget
    }

    enum Activity: Equatable { case idle, importing, unlocking, enrolling, syncing, refreshing, disconnecting }

    private(set) var status = SessionStatus()
    private(set) var activity = Activity.idle
    private(set) var lastError: String?
    private(set) var lastSync: SyncReport?
    private(set) var lastSyncDate: Date?
    private(set) var serverReachability: ServerReachabilityState = .unknown
    private(set) var devices: [DeviceRosterItem] = []
    private(set) var devicesLoaded = false
    private(set) var relay: RelayWakeClient?
    var relayOrigin = UserDefaults.standard.string(forKey: "peppy.relay-origin") ?? ""
    enum WakeStatus: Equatable { case settingUp, registered, retrying, paused, unavailable }
    private(set) var wakeStatus = WakeStatus.settingUp
    private let wakePreferenceKey = "peppy.background-updates-preference"
    /// Observable mirror of the durable preference. Network/OS state never writes this value.
    private var wakePreferenceValue = "automatic"
    @ObservationIgnored private var wakeFence = WakeWorkFence()
    @ObservationIgnored private var wakeEnrollmentBinding: WakeEnrollmentBinding?
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
    /// Limits a newly received remote scan request to one extra bounded local pass.
    @ObservationIgnored var remoteScanFollowUpQueued = false
    @ObservationIgnored private var foregroundReachabilityTask: Task<Void, Never>?
    @ObservationIgnored private let foregroundReachabilityOwnership = ForegroundReachabilityLoopOwnership()
    @ObservationIgnored private var foregroundActive = false

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
        migrateWakePreference()
        wakePreferenceValue = UserDefaults.standard.string(forKey: wakePreferenceKey) ?? "automatic"
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
        if status.identity == nil { serverReachability = .unknown }
        await reconcileWakeRegistration()
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
        stopForegroundReachability()
        wakeFence.preferenceChanged()
        wakeEnrollmentBinding = nil
        let retiringRelay = relay
        relay = nil
        // Queue cleanup while the old identity's credential context is still available.
        try? await session.retireRelayWakeContexts()
        await retiringRelay?.revoke()
        await run(.disconnecting) { try await $0.disconnect() }
        if status.identity == nil {
            relay = nil
            UserDefaults.standard.set(false, forKey: "peppy.relay-opted-in")
            UserDefaults.standard.removeObject(forKey: "peppy.relay-origin")
            relayOrigin = ""
        }
        lastSync = nil
        lastSyncDate = nil
        if status.identity == nil { serverReachability = .unknown }
    }

    func disconnectLocalOnly() async {
        stopForegroundReachability()
        wakeFence.preferenceChanged()
        wakeEnrollmentBinding = nil
        let retiringRelay = relay
        relay = nil
        try? await session.retireRelayWakeContexts()
        await retiringRelay?.revoke()
        await run(.disconnecting) { try await $0.disconnectLocalOnly() }
        if status.identity == nil {
            relay = nil
            UserDefaults.standard.set(false, forKey: "peppy.relay-opted-in")
            UserDefaults.standard.removeObject(forKey: "peppy.relay-origin")
            relayOrigin = ""
            lastError = "Signed out locally only. This device credential may remain active on the server until it can be revoked."
            serverReachability = .unknown
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

    /// Pending-secret availability is intentionally all the UI learns; the secret never enters UI state.
    func hasPendingVaultSecret(origin: String) async -> Bool {
        (try? await session.hasPendingVaultSecret(origin: origin)) ?? false
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

    func cancelVaultRequest(origin: String) async -> Bool {
        activity = .enrolling
        defer { activity = .idle }
        do {
            try await session.cancelVaultRequest(origin: origin)
            lastError = nil
            return true
        } catch {
            lastError = (error as? ClientError)?.userMessage ?? "Could not cancel the vault request."
            return false
        }
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
            await reconcileWakeRegistration()
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

    enum OwnerVaultSecretRevealOutcome {
        case revealed(String)
        case cancelled
        case unavailable
        case failed
    }

    func revealOwnerVaultSecret() async -> OwnerVaultSecretRevealOutcome {
        #if canImport(LocalAuthentication)
        let context = LAContext()
        do {
            guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else {
                return .unavailable
            }
            try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: String(localized: "peppy.owner_phone_secret_reveal_cta", table: "Peppy"))
            return .revealed(try await session.ownerVaultSecret())
        } catch {
            if let error = error as? LAError,
               [.userCancel, .appCancel, .systemCancel].contains(error.code) {
                return .cancelled
            }
            return .failed
        }
        #else
        return .unavailable
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
            UserDefaults.standard.set(origin.serialized, forKey: "peppy.relay-origin")
            UserDefaults.standard.set(true, forKey: "peppy.relay-opted-in")
            wakePreferenceValue = "enabled"
            wakeFence.preferenceChanged()
            wakeEnrollmentBinding = nil
            UserDefaults.standard.set("enabled", forKey: wakePreferenceKey)
            relayOrigin = origin.serialized
            lastError = nil
            let retiringRelay = relay
            relay = nil
            Task {
                try? await session.retireRelayWakeContexts()
                await retiringRelay?.revoke()
                await reconcileWakeRegistration()
            }
        } catch { lastError = "Enter a canonical HTTPS relay origin." }
    }

    var backgroundUpdatesEnabled: Bool { wakePreferenceValue != "paused" }

    func setBackgroundUpdatesEnabled(_ enabled: Bool) {
        let preference = enabled ? "enabled" : "paused"
        wakePreferenceValue = preference
        wakeFence.preferenceChanged()
        wakeEnrollmentBinding = nil
        UserDefaults.standard.set(preference, forKey: wakePreferenceKey)
        let retiringRelay = relay
        if !enabled { relay = nil }
        Task {
            if enabled { await reconcileWakeRegistration() }
            else {
                wakeStatus = .paused
                // Persist retirement before attempting network work so this survives an offline
                // pause or a process death with no in-memory relay client.
                try? await session.retireRelayWakeContexts()
                await retiringRelay?.revoke()
                await drainRetiringWakeContexts()
            }
        }
    }

    /// APNs registration has no visible notification prompt. It occurs only for an eligible hosted
    /// enrollment and a non-paused durable preference.
    func reconcileWakeRegistration() async {
        wakeEnrollmentBinding = nil
        let existingRelay = relay
        relay = nil
        if status.identity == nil { status = (try? await session.open()) ?? status }
        guard backgroundUpdatesEnabled, let identity = status.identity else {
            // Retiring contexts outlive the active enrollment and preference. Drain them before
            // returning so opting out cannot strand a server route after a cold restart.
            await drainRetiringWakeContexts()
            if !backgroundUpdatesEnabled { wakeStatus = .paused }
            return
        }
        await registerEnabledWake(identity, captured: wakeFence.generation, existingRelay: existingRelay)
        // Every enabled exit sweeps retiring contexts: unavailable, failed probes, thrown errors and
        // publication. A pause that won the race already retired and drained, so it is not repeated.
        if backgroundUpdatesEnabled { await drainRetiringWakeContexts() }
    }

    private func registerEnabledWake(
        _ identity: EnrollmentIdentity,
        captured: UInt,
        existingRelay: RelayWakeClient?
    ) async {
        #if DEBUG
        let environment = "sandbox"
        #else
        let environment = "production"
        #endif
        do {
            let hosted = await session.isHostedWakeEligible(origin: identity.origin)
            let target: RelayWakeRegistrationTarget
            let wakeIdentity: RelayWakeIdentity
            if hosted {
                guard let configuration = try await session.hostedWakeConfiguration(environment: environment),
                      let configuredTarget = RelayWakeRegistrationTarget.hosted(configuration: configuration) else {
                    wakeStatus = .unavailable
                    return
                }
                target = configuredTarget
                wakeIdentity = RelayWakeIdentity(origin: identity.origin, vaultId: identity.vaultId, deviceId: identity.deviceId, environment: configuration.environment)
                guard await wakeWorkMayContinue(captured) else { return }
            } else {
                guard let configuredTarget = RelayWakeRegistrationTarget.custom(
                    backgroundUpdatesEnabled: backgroundUpdatesEnabled,
                    optedIn: UserDefaults.standard.bool(forKey: "peppy.relay-opted-in"),
                    persistedOrigin: UserDefaults.standard.string(forKey: "peppy.relay-origin"),
                    environment: environment
                ) else {
                    wakeStatus = .unavailable
                    return
                }
                target = configuredTarget
                wakeIdentity = RelayWakeIdentity(origin: identity.origin, vaultId: identity.vaultId, deviceId: identity.deviceId, environment: environment)
            }
            let supportsCandidates = try await session.relayWakeSupportsCandidates(origin: target.origin)
            // A capability answer that completes after a pause must not reactivate the context.
            guard await wakeWorkMayContinue(captured) else { return }
            // Hosted automatic registration requires replay-safe confirmation; explicit community
            // relays retain their legacy wire compatibility when capability is absent.
            guard !hosted || supportsCandidates else {
                wakeStatus = .unavailable
                return
            }
            let selectedRelay: RelayWakeClient
            if let existingRelay,
               await existingRelay.isBound(to: wakeIdentity, relayOrigin: target.origin.serialized) {
                selectedRelay = existingRelay
            } else {
                let publisher = try await session.capturedRelayWakeRoutePublisher()
                selectedRelay = RelayWakeClient(origin: target.origin, transport: URLSessionTransport(), secure: KeychainSecureStore(), publisher: publisher, identity: wakeIdentity, supportsCandidates: supportsCandidates)
                try await selectedRelay.migrateLegacyRouteIfNeeded()
            }
            // Every enabled reconcile reactivates this identity's context, including a bound client
            // resumed after a pause. This clears any retiring duplicate, so the drain sweep cannot
            // revoke the route that is about to be published.
            guard await wakeWorkMayContinue(captured) else { return }
            try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: target.origin.serialized)
            // A pause that landed during registration retires the context it just made live.
            guard await wakeWorkMayContinue(captured) else { return }
            wakeEnrollmentBinding = WakeEnrollmentBinding(identity: wakeIdentity, target: target)
            relay = selectedRelay
            await selectedRelay.retryCleanup()
            _ = try await selectedRelay.publishConfirmedRoute()
            await refreshWakeStatus()
            #if canImport(UIKit)
            UIApplication.shared.registerForRemoteNotifications()
            #endif
        } catch {
            wakeStatus = .retrying
        }
    }

    /// Re-reads the preference after an await. A pause retires and drains whatever this reconcile may
    /// have made live; a newer preference generation leaves the work to its own reconcile.
    private func wakeWorkMayContinue(_ captured: UInt) async -> Bool {
        switch wakeFence.checkpoint(captured: captured, enabled: backgroundUpdatesEnabled) {
        case .proceed:
            return true
        case .superseded:
            return false
        case .retirePaused:
            wakeStatus = .paused
            let retiringRelay = relay
            relay = nil
            try? await session.retireRelayWakeContexts()
            await retiringRelay?.revoke()
            await drainRetiringWakeContexts()
            return false
        }
    }

    private func drainRetiringWakeContexts() async {
        await session.drainRetiringRelayWakeContexts(relayTransport: URLSessionTransport(), secure: KeychainSecureStore())
    }

    func receiveAPNSToken(_ token: String) async {
        guard backgroundUpdatesEnabled else { return }
        // A verified binding from the last reconcile is reused. Reconciling again here would call
        // registerForRemoteNotifications, which returns a token and re-enters this callback.
        if wakeEnrollmentBinding == nil || relay == nil { await reconcileWakeRegistration() }
        guard !Task.isCancelled,
              let binding = wakeEnrollmentBinding,
              let relay,
              await relay.isBound(to: binding.identity, relayOrigin: binding.target.origin.serialized) else {
            return
        }
        do {
            _ = try await relay.beginEnrollment(deviceToken: token)
            await refreshWakeStatus()
        } catch {
            wakeStatus = .retrying
        }
    }

    func refreshWakeStatus() async {
        guard backgroundUpdatesEnabled else { wakeStatus = .paused; return }
        guard let status = try? await relay?.registrationStatus() else { wakeStatus = .unavailable; return }
        switch status.phase {
        case .registered: wakeStatus = .registered
        case .pending, .confirmedUnpublished: wakeStatus = .settingUp
        case .paused: wakeStatus = .paused
        case .idle: wakeStatus = .unavailable
        }
    }

    func apnsRegistrationFailed() { if backgroundUpdatesEnabled { wakeStatus = .retrying } }

    func wakeSync() async -> Bool {
        // A background APNs launch has no SwiftUI `.task` to open the session first.
        guard let opened = try? await session.open(), opened.databaseOpen, opened.keysUnlocked else { return false }
        do {
            _ = try await session.syncOnce()
            await publishServerReachability()
            return true
        } catch {
            await publishServerReachability()
            return false
        }
    }

    func wakeContacts() async -> Bool {
        guard contacts.enabled else { return true }
        await runContactsPass(.backgroundRefresh)
        return !Task.isCancelled
    }

    private func migrateWakePreference() {
        guard UserDefaults.standard.object(forKey: wakePreferenceKey) == nil else { return }
        if UserDefaults.standard.object(forKey: "peppy.relay-opted-in") == nil {
            UserDefaults.standard.set("automatic", forKey: wakePreferenceKey)
        } else if UserDefaults.standard.bool(forKey: "peppy.relay-opted-in") {
            UserDefaults.standard.set("enabled", forKey: wakePreferenceKey)
        } else {
            UserDefaults.standard.set("paused", forKey: wakePreferenceKey)
        }
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
        await publishServerReachability()
        await runContactsPass(.foreground)
    }

    /// Starts foreground-only observation for the current enrollment. A full sync supplies the
    /// observation when unlocked; locked vaults use one bounded authenticated probe instead.
    func setForegroundActive(_ active: Bool) {
        foregroundActive = active
        guard active else { stopForegroundReachability(); return }
        startForegroundReachabilityIfNeeded()
    }

    private func startForegroundReachabilityIfNeeded() {
        guard foregroundActive, foregroundReachabilityTask == nil, status.identity != nil else { return }
        let token = foregroundReachabilityOwnership.start()
        foregroundReachabilityTask = Task { [weak self] in
            await self?.foregroundReachabilityLoop(token: token)
        }
    }

    private func stopForegroundReachability() {
        foregroundReachabilityTask?.cancel()
        foregroundReachabilityTask = nil
        foregroundReachabilityOwnership.stop()
    }

    private func foregroundReachabilityLoop(token: Int) async {
        defer {
            let ownsCurrentTask = foregroundReachabilityOwnership.isCurrent(token)
            foregroundReachabilityOwnership.finish(token)
            if ownsCurrentTask { foregroundReachabilityTask = nil }
        }
        var delay: UInt64 = 30
        while !Task.isCancelled, status.identity != nil {
            if status.keysUnlocked {
                await syncInForeground()
            } else {
                do { try await session.probeServerReachability() }
                catch ClientError.syncInProgress, ClientError.canceled {}
                catch is CancellationError {}
                catch {}
            }
            await publishServerReachability()
            guard serverReachability != .available else { return }
            do {
                try await Task.sleep(for: .seconds(delay))
            } catch { return }
            delay = min(delay * 2, 300)
        }
    }

    func publishServerReachability() async {
        serverReachability = await session.serverReachability()
        if serverReachability == .unavailable { startForegroundReachabilityIfNeeded() }
    }

    private func run(_ activity: Activity, _ body: (NativeSession) async throws -> SessionStatus) async {
        self.activity = activity
        defer { self.activity = .idle }
        do {
            let previousIdentity = status.identity
            status = try await body(session)
            if status.identity != previousIdentity {
                serverReachability = .unknown
                wakeFence.preferenceChanged()
                wakeEnrollmentBinding = nil
                relay = nil
            }
            lastError = nil
            enrollmentBlocked = false
            await reconcileWakeRegistration()
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
