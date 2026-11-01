import Foundation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

/// Hosted-account failures intentionally contain no provider token, bearer, or passphrase.
public enum HostedAccountClientError: Error, Equatable, Sendable {
    case sessionExpired, unavailable, wrongAccount, wrongPassphrase, entitlementRequired, provisioningInProgress, invalidResponse
}

/// Actor owning the native hosted-account bearer and encrypted provisioning checkpoint.
public actor HostedAccountClient {
    private struct StoredSession: Codable, Equatable { let accountID: String; let bearer: String; let expiresAt: Date }
    private static let sessionLifetime: TimeInterval = 2_592_000
    private static let renewalThrottle: TimeInterval = 15 * 60
    private static let failedRenewalBackoff: TimeInterval = 60
    private static let sessionMutationLock = NSLock()
    private let transport: any HTTPTransport
    private let secureStore: any SecureStore
    private let origin: ServerOrigin
    private let sessionKey: String
    private let checkpointKey: String
    private var generation = 0
    private var provisioning = false
    private var renewalTask: Task<Void, Error>?
    private var lastFailedRenewal: Date?

    /// The hosted account origin. Billing and account links use it, and it is trusted for account notices.
    public static let productionOrigin = "https://peppy.pro"

    public init(transport: any HTTPTransport = URLSessionTransport(), secureStore: any SecureStore = KeychainSecureStore(), origin: String = HostedAccountClient.productionOrigin) throws {
        self.transport = transport; self.secureStore = secureStore
        self.origin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: false)
        sessionKey = "hosted-account-session:\(self.origin.serialized)"
        checkpointKey = HostedProvisioningStorage.checkpointAccount(origin: self.origin.serialized)
    }

    public func availableProviders() async throws -> [String] {
        let response = try await request("GET", "/hosted/v1/auth/config")
        guard response.status == 200, let object = try JSONSerialization.jsonObject(with: response.body) as? [String: Any],
              Set(object.keys) == ["available_providers"], let values = object["available_providers"] as? [String],
              Set(values).isSubset(of: ["google", "apple"]), Set(values).count == values.count else { throw HostedAccountClientError.unavailable }
        return values
    }

    public func beginGoogleSignIn() async throws -> NativeHostedLoginAttempt {
        let response = try await request("POST", "/hosted/v1/auth/attempts", body: try hostedLoginRequest(provider: "google"))
        guard response.status == 200 || response.status == 201 else { throw failure(response) }
        return try parseHostedLoginAttempt(json: try json(response.body))
    }

    public func finishGoogleSignIn(attemptID: String, idToken: String) async throws -> NativeHostedAccount {
        generation &+= 1
        let expected = generation
        let body = try hostedSessionRequest(attemptId: attemptID, idToken: idToken)
        let response = try await request("POST", "/hosted/v1/auth/session", body: body)
        guard response.status == 200 else { throw failure(response) }
        let parsed = try parseHostedSession(json: try json(response.body))
        try Task.checkCancellation()
        let stored = StoredSession(accountID: parsed.accountId(), bearer: parsed.bearerToken(), expiresAt: Date().addingTimeInterval(TimeInterval(parsed.expiresInSeconds())))
        try Self.sessionMutationLock.withLock {
            guard expected == generation else { throw CancellationError() }
            try secureStore.upsert(sessionKey, try JSONEncoder().encode(stored))
        }
        lastFailedRenewal = nil
        return try await account(using: stored, expectedGeneration: expected)
    }

    public func account() async throws -> NativeHostedAccount {
        let stored = try loadSession()
        return try await account(using: stored, expectedGeneration: generation)
    }

    /// Renews the hosted bearer only after a concrete user action. Foreground lifecycle work must
    /// not call this method: a local expiry can be skewed or reflect a lost renewal response.
    public func renewSessionOnUserInteraction() async throws {
        if let renewalTask { return try await renewalTask.value }
        let task = Task { try await self.renewObservedSession() }
        renewalTask = task
        defer { renewalTask = nil }
        try await task.value
    }

    /// Native-only enrollment authorization. It is deliberately available only
    /// for the pinned hosted origin and never crosses a UI boundary.
    func enrollmentSessionBearer() throws -> String {
        guard origin.serialized == "https://peppy.pro" else { throw HostedAccountClientError.unavailable }
        let session = try loadSession()
        guard session.expiresAt > Date() else { throw HostedAccountClientError.sessionExpired }
        return session.bearer
    }

    public func signOut() async throws {
        generation &+= 1
        // Capture the session before this actor yields for best-effort revocation.
        // If a newer login is installed during DELETE, it will be preserved because we delete before awaiting.
        let stored = try Self.sessionMutationLock.withLock { () throws -> StoredSession? in
            let stored = try? rawSession()
            try secureStore.delete(sessionKey)
            return stored
        }
        if let stored { _ = try? await request("DELETE", "/hosted/v1/auth/session", bearer: stored.bearer) }
    }

    public func pendingProvisioning() async throws -> NativeHostedProvisioning? {
        guard let data = try HostedProvisioningStorage.withLock({ try secureStore.read(checkpointKey) }) else { return nil }
        let session = try loadSession()
        do { return try restoreHostedProvisioning(checkpointBytes: data, expectedOrigin: origin.serialized, expectedAccountId: session.accountID) }
        catch { throw HostedAccountClientError.wrongAccount }
    }

    public func prepareVault(passphrase: String) async throws -> NativeHostedProvisioningView {
        guard !provisioning else { throw HostedAccountClientError.provisioningInProgress }; provisioning = true; defer { provisioning = false }
        let expected = generation
        let account = try await account()
        guard expected == generation else { throw CancellationError() }
        if let existing = try await pendingProvisioning() {
            try Task.checkCancellation()
            guard expected == generation else { throw CancellationError() }
            guard existing.view().accountId == account.accountId else { throw HostedAccountClientError.wrongAccount }
            guard try existing.passphraseMatches(passphrase: passphrase) else { throw HostedAccountClientError.wrongPassphrase }
            try retainPendingSecret(passphrase, accountID: account.accountId)
            return existing.view()
        }
        guard account.access == "read_write" else { throw HostedAccountClientError.entitlementRequired }
        guard expected == generation else { throw CancellationError() }
        guard account.vaultId == nil else { throw HostedAccountClientError.provisioningInProgress }
        let item = try prepareHostedProvisioning(
            origin: origin.serialized,
            accountId: account.accountId,
            passphrase: passphrase,
            clientKind: .ios,
            deviceName: "Peppy iOS"
        )
        try Task.checkCancellation()
        // The secret is retained only after every prepare/entitlement gate passed, immediately
        // before its durable checkpoint. A failed prepare cannot orphan a blocking secret.
        let checkpoint = try item.checkpoint()
        try HostedProvisioningStorage.withLock {
            try retainPendingSecretLocked(passphrase, accountID: account.accountId)
            try secureStore.upsert(checkpointKey, checkpoint)
        }
        return item.view()
    }

    public func completeVault(passphrase: String) async throws -> Data {
        guard !provisioning else { throw HostedAccountClientError.provisioningInProgress }
        provisioning = true; defer { provisioning = false }
        let expected = generation
        guard let item = try await pendingProvisioning() else { throw HostedAccountClientError.wrongAccount }
        let session = try loadSession()
        guard try item.passphraseMatches(passphrase: passphrase) else { throw HostedAccountClientError.wrongPassphrase }
        try retainPendingSecret(passphrase, accountID: session.accountID)
        let view = item.view()
        guard view.accountId == session.accountID else { throw HostedAccountClientError.wrongAccount }
        func stillCurrent() throws {
            try Task.checkCancellation()
            guard expected == generation, (try rawSession())?.accountID == session.accountID else { throw CancellationError() }
        }
        if !item.hasGrant() {
            let grant = try await request("POST", "/hosted/v1/provisioning", body: try item.grantRequest(), bearer: session.bearer)
            try stillCurrent()
            guard grant.status == 200 || grant.status == 201 else { throw failure(grant) }
            try item.acceptGrant(responseJson: try json(grant.body)); try stillCurrent()
            let checkpoint = try item.checkpoint()
            try HostedProvisioningStorage.withLock { try secureStore.upsert(checkpointKey, checkpoint) }
        }
        let body = try item.completeRequest()
        var complete = try await request("POST", "/hosted/v1/provisioning/complete", body: body, bearer: session.bearer)
        try stillCurrent()
        // An expired grant may be replaced only for the same still-pending operation with no vault.
        if !(complete.status == 200 || complete.status == 201) {
            let account = try await account(using: session, expectedGeneration: expected)
            try stillCurrent()
            guard account.vaultId == nil, account.access == "read_write" else { throw failure(complete) }
            let fresh = try await request("POST", "/hosted/v1/provisioning", body: try item.grantRequest(), bearer: session.bearer)
            try stillCurrent()
            guard fresh.status == 200 || fresh.status == 201 else { throw failure(fresh) }
            try item.acceptGrant(responseJson: try json(fresh.body)); try stillCurrent()
            let checkpoint = try item.checkpoint()
            try HostedProvisioningStorage.withLock { try secureStore.upsert(checkpointKey, checkpoint) }
            complete = try await request("POST", "/hosted/v1/provisioning/complete", body: try item.completeRequest(), bearer: session.bearer)
            try stillCurrent()
        }
        guard complete.status == 200 || complete.status == 201 else { throw failure(complete) }
        let credential = try item.credentialJson(completeResponseJson: try json(complete.body))
        try stillCurrent()
        return Data(credential.utf8)
    }

    /// Removes the checkpoint only once the verified binding for its vault and device holds the
    /// passphrase. It never writes a pending secret: the binding already consumed that copy, and
    /// a missing or conflicting binding fails closed with the checkpoint kept for recovery.
    public func acknowledgeEnrollment(identity: EnrollmentIdentity, passphrase: String) async throws {
        guard let checkpoint = try HostedProvisioningStorage.withLock({ try secureStore.read(checkpointKey) }) else { return }
        let session = try loadSession()
        let item: NativeHostedProvisioning
        do {
            item = try restoreHostedProvisioning(
                checkpointBytes: checkpoint,
                expectedOrigin: origin.serialized,
                expectedAccountId: session.accountID
            )
        } catch {
            throw HostedAccountClientError.wrongAccount
        }
        let view = item.view()
        guard view.vaultId == identity.vaultId, view.deviceId == identity.deviceId,
              identity.origin == origin.serialized else { throw HostedAccountClientError.wrongAccount }
        try HostedProvisioningStorage.withLock {
            guard try secureStore.read(checkpointKey) == checkpoint else { throw HostedAccountClientError.wrongAccount }
            guard let bound = try VaultSecretStore(secure: secureStore).read(identity) else {
                throw HostedAccountClientError.unavailable
            }
            guard bound == passphrase else { throw HostedAccountClientError.wrongPassphrase }
            try secureStore.delete(checkpointKey)
        }
    }

    private func account(using session: StoredSession, expectedGeneration: Int) async throws -> NativeHostedAccount {
        guard session.expiresAt > Date() else { throw HostedAccountClientError.sessionExpired }
        let response = try await request("GET", "/hosted/v1/account", bearer: session.bearer)
        guard expectedGeneration == generation else { throw CancellationError() }
        if response.status == 401 {
            try deleteObservedSession(session, expectedGeneration: expectedGeneration)
            throw HostedAccountClientError.sessionExpired
        }
        guard response.status == 200 else { throw failure(response) }
        return try parseHostedAccount(json: try json(response.body), expectedAccountId: session.accountID)
    }
    private func rawSession() throws -> StoredSession? {
        guard let data = try secureStore.read(sessionKey) else { return nil }
        guard let value = try? JSONDecoder().decode(StoredSession.self, from: data) else { throw HostedAccountClientError.sessionExpired }
        return value
    }
    private func loadSession() throws -> StoredSession {
        guard let value = try rawSession(), value.expiresAt > Date() else { throw HostedAccountClientError.sessionExpired }
        return value
    }
    private func renewObservedSession() async throws {
        let now = Date()
        if let lastFailedRenewal, now.timeIntervalSince(lastFailedRenewal) < Self.failedRenewalBackoff { return }
        let session: StoredSession
        do {
            guard let stored = try rawSession() else { throw HostedAccountClientError.sessionExpired }
            session = stored
        } catch {
            lastFailedRenewal = now
            throw error
        }
        guard session.expiresAt.timeIntervalSince(now) <= Self.sessionLifetime - Self.renewalThrottle else { return }

        let expectedGeneration = generation
        do {
            let response = try await request("POST", "/hosted/v1/auth/session/renew", bearer: session.bearer)
            guard expectedGeneration == generation else { throw CancellationError() }
            if response.status == 401 {
                try deleteObservedSession(session, expectedGeneration: expectedGeneration)
                throw HostedAccountClientError.sessionExpired
            }
            guard response.status == 200 else { throw failure(response) }
            let lifetime = try parseHostedSessionRenewal(json: try json(response.body), expectedAccountId: session.accountID)
            try Task.checkCancellation()
            let renewed = StoredSession(
                accountID: session.accountID,
                bearer: session.bearer,
                expiresAt: Date().addingTimeInterval(TimeInterval(lifetime))
            )
            try Self.sessionMutationLock.withLock {
                guard expectedGeneration == generation, try rawSession() == session else { throw CancellationError() }
                try secureStore.upsert(sessionKey, try JSONEncoder().encode(renewed))
            }
            lastFailedRenewal = nil
        } catch {
            if expectedGeneration == generation, (try? rawSession()) == session { lastFailedRenewal = now }
            throw error
        }
    }
    private func deleteObservedSession(_ session: StoredSession, expectedGeneration: Int) throws {
        try Self.sessionMutationLock.withLock {
            guard expectedGeneration == generation, try rawSession() == session else { throw CancellationError() }
            try secureStore.delete(sessionKey)
        }
    }

    /// The account's pending secret from an interrupted new-vault prepare (for example a failed
    /// checkpoint write). The UI reuses it rather than generating a phrase that would fail closed.
    /// Storage errors throw; nothing is written.
    func candidatePendingSecret() throws -> String? {
        let session = try loadSession()
        return try HostedProvisioningStorage.withLock {
            try VaultSecretStore(secure: secureStore).pendingSecret(origin: pendingSecretScope(session.accountID))
        }
    }
    func bindVerifiedEnrollmentSecret(_ passphrase: String, identity: EnrollmentIdentity) throws {
        let session = try loadSession()
        let scope = pendingSecretScope(session.accountID)
        try HostedProvisioningStorage.withLock {
            try retainPendingSecretLocked(passphrase, accountID: session.accountID)
            guard VaultSecretStore(secure: secureStore).storeBinding(passphrase, for: identity, origin: scope) else {
                throw HostedAccountClientError.unavailable
            }
        }
    }
    private func pendingSecretScope(_ accountID: String) -> String {
        HostedProvisioningStorage.pendingScope(origin: origin.serialized, accountID: accountID)
    }
    /// Preserves a matching protected value and fails closed on a conflicting nonempty value.
    private func retainPendingSecret(_ secret: String, accountID: String) throws {
        try HostedProvisioningStorage.withLock {
            try retainPendingSecretLocked(secret, accountID: accountID)
        }
    }
    private func retainPendingSecretLocked(_ secret: String, accountID: String) throws {
        let secrets = VaultSecretStore(secure: secureStore)
        let scope = pendingSecretScope(accountID)
        if let existing = try secrets.pendingSecret(origin: scope) {
            guard existing == secret else { throw HostedAccountClientError.wrongPassphrase }
        } else {
            try secrets.savePending(secret, origin: scope)
        }
    }
    private func request(_ method: String, _ path: String, body: String? = nil, bearer: String? = nil) async throws -> HTTPResponse {
        try Task.checkCancellation()
        var headers = ["Accept": "application/json"]; if let bearer { headers["Authorization"] = "Bearer \(bearer)" }; if body != nil { headers["Content-Type"] = "application/json" }
        let result = try await transport.send(HTTPRequest(method: method, url: origin.url(path), headers: headers, body: body.map { Data($0.utf8) }, maxResponseBytes: 64 * 1024))
        try Task.checkCancellation()
        guard origin.contains(result.url) else { throw ClientError.originMismatch }; return result
    }
    private func json(_ data: Data) throws -> String { guard let value = String(data: data, encoding: .utf8), data.count <= 64 * 1024 else { throw HostedAccountClientError.invalidResponse }; return value }
    private func failure(_ response: HTTPResponse) -> Error { response.status == 401 ? HostedAccountClientError.sessionExpired : (response.status == 403 ? HostedAccountClientError.entitlementRequired : HostedAccountClientError.unavailable) }
}
