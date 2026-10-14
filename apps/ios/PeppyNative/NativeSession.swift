import Foundation
import CryptoKit
#if canImport(PeppyBindings)
import PeppyBindings
#endif

/// What the UI may show. Opening the encrypted database does not imply the vault keys are unlocked.
public struct SessionStatus: Equatable, Sendable {
    public var identity: EnrollmentIdentity?
    public var role: String?
    public var keyEpoch: UInt32?
    public var databaseOpen = false
    public var keysUnlocked = false
    /// Stored key caches that the core rejected while reopening (kept, not deleted).
    public var rejectedKeyCaches = 0
    /// Nil when the core could not list conversations.
    public var conversations: Int?
}

public struct DeviceRosterItem: Identifiable, Equatable, Sendable {
    public let id: String
    public let role: String
    public let revoked: Bool
    public let keyEpoch: UInt32
}

public struct PhoneEnrollmentStatus: Equatable, Sendable {
    public let verificationCode: String?
    public let awaitingOwner: Bool
    public let requiresSharedSecret: Bool
}

public struct SelfHostedInstanceDiscovery: Equatable, Sendable {
    public let origin: String
    public let setupCodeRequired: Bool
    public let claimed: Bool
}

public struct VaultRequestProgress: Equatable, Sendable {
    public let origin: String
    public let requestID: String
    public let displayName: String
    public let state: String
    public let accepted: Bool
    public let hasPreparedOwnerMaterial: Bool
    /// True only when the protected pending copy of the prepared vault secret is present (a read error throws instead).
    public var hasStoredVaultSecret = false
}

private struct InstanceDiscoveryResponse: Decodable {
    let mode: String
    let claimed: Bool
    let setupRequired: Bool?

    enum CodingKeys: String, CodingKey {
        case mode, claimed
        case setupRequired = "setup_required"
    }
}

/// Owns the single process-wide `NativeClient`, its Keychain-held secrets and foreground sync.
/// All message state lives in the Rust core; this actor only moves opaque values.
///
/// Enrollment changes (import, refresh, disconnect) are serialized: one runs at a time, and every
/// identity decision is re-checked on the actor after the network await, right before it is saved.
public actor NativeSession {
    private let store: EnrollmentStore
    private let transport: any HTTPTransport
    private let databaseDirectory: URL
    private let allowLoopbackHTTP: Bool
    private var client: NativeClient?
    private var record: EnrollmentRecord?
    private var status = SessionStatus()
    private var syncing = false
    private var changingEnrollment = false
    private struct PhoneClaimContext: Sendable {
        let origin: String
        let invitationID: String
        let method: EnrollmentMethod
        let host: EnrollmentHost
    }

    private var phoneProducer: NativeEnrollmentRequestProducer?
    private var phoneClaimContext: PhoneClaimContext?
    private var phoneConfirmationRequest: String?
    private var vaultRequestProducer: NativeEnrollmentRequestProducer?
    private var vaultRequestOrigin: String?
    private var instanceSession: (origin: String, token: String, vaultID: String)?

    public init(
        secureStore: any SecureStore,
        transport: any HTTPTransport,
        databaseDirectory: URL,
        allowLoopbackHTTP: Bool
    ) {
        store = EnrollmentStore(secure: secureStore)
        self.transport = transport
        self.databaseDirectory = databaseDirectory
        self.allowLoopbackHTTP = allowLoopbackHTTP
    }

    // MARK: Enrollment

    /// Reads a credential file (bounded), verifies it against its server and persists it.
    public func importCredential(file: URL) async throws -> SessionStatus {
        let data: Data
        do {
            let handle = try FileHandle(forReadingFrom: file)
            defer { try? handle.close() }
            data = try handle.read(upToCount: DeviceCredential.maxFileBytes + 1) ?? Data()
        } catch {
            throw ClientError.invalidCredential("unreadable file")
        }
        return try await importCredential(data)
    }

    /// Imports a credential for a new enrollment, or replaces the token of the active one
    /// (same origin, vault and device). A different active enrollment must be disconnected first.
    public func importCredential(_ data: Data) async throws -> SessionStatus {
        let credential = try DeviceCredential.parse(data, allowLoopbackHTTP: allowLoopbackHTTP)
        return try await changeEnrollment {
            try self.requireAdoptable(credential.identity)
            let verified = try await EnrollmentVerifier(transport: self.transport)
                .verify(origin: credential.origin, token: credential.token, identity: credential.identity)
            // Re-checked after the await: another change may have completed meanwhile.
            try Task.checkCancellation()
            try self.requireAdoptable(credential.identity)
            try self.store.activate(verified, token: credential.token)
            return try self.adopt(verified)
        }
    }

    /// Re-reads the active enrollment's profile, epoch and role from the server with the stored token.
    public func refreshEnrollment() async throws -> SessionStatus {
        try await changeEnrollment {
            guard let identity = try self.store.activeIdentity() else { throw ClientError.notEnrolled }
            let verified = try await EnrollmentVerifier(transport: self.transport).verify(
                origin: try ServerOrigin(canonical: identity.origin, allowLoopbackHTTP: self.allowLoopbackHTTP),
                token: try self.store.token(identity),
                identity: identity
            )
            guard try self.store.activeIdentity() == identity else { throw ClientError.identityMismatch }
            try self.store.saveRecord(verified)
            return try self.adopt(verified)
        }
    }

    /// Claims an unowned community instance using Rust-generated request and
    /// identity objects. The credential is stored only after the verified root
    /// package has activated the native core.
    public func claimInstance(origin: String, setupToken: String, username: String, adminPassword: String, passphrase: String, deviceName: String) async throws -> SessionStatus {
        try await changeEnrollment {
            let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: self.allowLoopbackHTTP)
            let producer = try NativeEnrollmentRequestProducer(origin: serverOrigin.serialized, deviceName: deviceName)
            let secrets = VaultSecretStore(secure: self.store.secure)
            // A stored pending secret wins, so a retry after a rejected claim never switches to a new secret.
            let secret = try secrets.candidatePendingSecret(origin: serverOrigin.serialized, entered: passphrase)
            try secrets.savePending(secret, origin: serverOrigin.serialized)
            let request = try producer.createOwnerRequest(setupToken: setupToken, username: username, adminPassword: adminPassword, passphrase: secret)
            let response = try await EnrollmentHost(origin: serverOrigin, transport: self.transport, accountSession: try await self.hostedSession(for: serverOrigin)).submitOwnerClaim(request)
            let enrollment = try producer.finishOwnerAdmission(response, passphrase: secret)
            return try self.activateVerifiedEnrollment(enrollment, bindingSecret: secret)
        }
    }

    /// Discovers self-hosted first-claim policy before accepting policy-dependent credentials.
    public func discoverSelfHostedInstance(origin: String) async throws -> SelfHostedInstanceDiscovery {
        let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: allowLoopbackHTTP)
        let response = try await EnrollmentHost(origin: serverOrigin, transport: transport).discoverInstance()
        guard let instance = try? JSONDecoder().decode(InstanceDiscoveryResponse.self, from: Data(response.utf8)),
              instance.mode == "community" else {
            throw ClientError.invalidResponse("instance discovery")
        }
        return SelfHostedInstanceDiscovery(
            origin: serverOrigin.serialized,
            setupCodeRequired: instance.setupRequired != false,
            claimed: instance.claimed
        )
    }

    /// Completes direct manual shared-secret admission. The server never
    /// receives the secret: the generated client creates the proof locally.
    public func joinWithSharedSecret(origin: String, vaultID: String?, vaultOwner: String?, passphrase: String, deviceName: String) async throws -> SessionStatus {
        try await changeEnrollment {
            let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: self.allowLoopbackHTTP)
            let producer = try NativeEnrollmentRequestProducer(origin: serverOrigin.serialized, deviceName: deviceName)
            let session = self.instanceSession?.origin == serverOrigin.serialized ? self.instanceSession?.token : nil
            let host = EnrollmentHost(origin: serverOrigin, transport: self.transport, accountSession: try await self.hostedSession(for: serverOrigin), instanceSession: session)
            let secret = try VaultSecretStore(secure: self.store.secure).sharedSecretForJoin(origin: serverOrigin.serialized, entered: passphrase)
            let request = try producer.manualChallengeRequest(vaultID: vaultID, vaultOwner: vaultOwner)
            let challenge = try await host.requestManualChallenge(request)
            guard let id = (try JSONSerialization.jsonObject(with: Data(challenge.utf8)) as? [String: Any])?["enrollment_id"] as? String else {
                throw ClientError.invalidResponse("manual challenge")
            }
            let complete = try producer.completeManualChallengeRequest(challenge: challenge, passphrase: secret)
            let response = try await host.completeManualChallenge(id: id, request: complete)
            let enrollment = try producer.finishManualAdmission(response, passphrase: secret)
            return try self.activateVerifiedEnrollment(enrollment, bindingSecret: secret)
        }
    }

    /// A Keychain read failure is propagated so callers can fail closed rather than enabling an empty submission.
    public func hasPendingVaultSecret(origin: String) throws -> Bool {
        let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: allowLoopbackHTTP)
        return try VaultSecretStore(secure: store.secure).pendingSecret(origin: serverOrigin.serialized) != nil
    }

    /// Password login is scoped to one instance vault. It is kept in memory only and does not create a browser cookie.
    public func signIn(origin: String, username: String, password: String) async throws {
        let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: allowLoopbackHTTP)
        let host = EnrollmentHost(origin: serverOrigin, transport: transport)
        let response = try await host.signIn(username: username, password: password)
        guard let session = try JSONSerialization.jsonObject(with: Data(response.utf8)) as? [String: Any],
              let token = session["session_token"] as? String, !token.isEmpty else {
            throw ClientError.invalidResponse("instance login")
        }
        let accountHost = EnrollmentHost(origin: serverOrigin, transport: transport, instanceSession: token)
        let account = try await accountHost.instanceAccount()
        guard let value = try JSONSerialization.jsonObject(with: Data(account.utf8)) as? [String: Any],
              let vaultID = value["vault_id"] as? String else { throw ClientError.invalidResponse("instance account") }
        instanceSession = (origin: serverOrigin.serialized, token: token, vaultID: vaultID)
    }

    /// Starts an invite request only after its opaque Rust checkpoint has been protected locally.
    public func beginVaultRequest(inviteLink: String, displayName: String, expectedOrigin: String?, deviceName: String) async throws -> VaultRequestProgress {
        let invite = try parseVaultInviteLink(link: inviteLink)
        let origin = try ServerOrigin(canonical: invite.origin, allowLoopbackHTTP: allowLoopbackHTTP)
        if let expectedOrigin, !expectedOrigin.isEmpty,
           try ServerOrigin(canonical: expectedOrigin, allowLoopbackHTTP: allowLoopbackHTTP).serialized != origin.serialized {
            throw ClientError.originMismatch
        }
        let producer = try restoredVaultRequestProducer(for: origin.serialized)
            ?? NativeEnrollmentRequestProducer(origin: origin.serialized, deviceName: deviceName)
        let request = try producer.beginVaultRequest(origin: origin.serialized, inviteToken: invite.inviteToken, displayName: displayName)
        try saveVaultRequestCheckpoint(producer, origin: origin.serialized)
        let host = EnrollmentHost(origin: origin, transport: transport, accountSession: try await hostedSession(for: origin))
        do {
            let response = try await host.submitVaultRequest(request)
            try producer.acceptVaultRequestCreated(response)
            try saveVaultRequestCheckpoint(producer, origin: origin.serialized)
            return try vaultProgress(producer: producer, state: "pending")
        } catch {
            if isDefinitiveInviteSubmissionFailure(error) { try clearVaultRequest(origin: origin.serialized, producer: producer) }
            throw error
        }
    }

    public func vaultInviteOrigin(link: String) throws -> String {
        try ServerOrigin(canonical: parseVaultInviteLink(link: link).origin, allowLoopbackHTTP: allowLoopbackHTTP).serialized
    }

    public func pendingVaultRequest(origin: String) throws -> VaultRequestProgress? {
        let producer = try restoredVaultRequestProducer(for: origin)
        guard let producer, try producer.pendingVaultRequest() != nil else { return nil }
        return try vaultProgress(producer: producer, state: try producer.hasPreparedOwnerMaterial() ? "approved" : "pending")
    }

    /// Performs one foreground long-poll. The transport request timeout is 20 seconds while the server wait is 15.
    public func vaultRequestStatus(origin: String, knownState: String? = "pending") async throws -> VaultRequestProgress {
        let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: allowLoopbackHTTP)
        guard let producer = try restoredVaultRequestProducer(for: serverOrigin.serialized),
              let pending = try producer.pendingVaultRequest() else { throw ClientError.notEnrolled }
        let host = EnrollmentHost(origin: serverOrigin, transport: transport, accountSession: try await hostedSession(for: serverOrigin))
        if try !producer.vaultRequestAccepted() {
            let request = try producer.beginVaultRequest(origin: serverOrigin.serialized, inviteToken: "", displayName: "")
            try saveVaultRequestCheckpoint(producer, origin: serverOrigin.serialized)
            do {
                try producer.acceptVaultRequestCreated(await host.submitVaultRequest(request))
                try saveVaultRequestCheckpoint(producer, origin: serverOrigin.serialized)
            } catch {
                if isDefinitiveInviteSubmissionFailure(error) { try clearVaultRequest(origin: serverOrigin.serialized, producer: producer) }
                throw error
            }
        }
        let response: String
        do {
            response = try await host.vaultRequestStatus(id: pending.requestId, secret: try producer.vaultRequestHeader(), knownState: knownState)
        } catch {
            if case .server(404, "request_not_found") = ClientError.wrap(error) {
                try clearVaultRequest(origin: serverOrigin.serialized, producer: producer)
                return VaultRequestProgress(origin: pending.origin, requestID: pending.requestId, displayName: pending.displayName, state: "expired", accepted: false, hasPreparedOwnerMaterial: false)
            }
            throw error
        }
        let state = try producer.parseVaultRequestStatus(response)
        if state == "denied" || state == "expired" {
            try clearVaultRequest(origin: serverOrigin.serialized, producer: producer)
        } else {
            try saveVaultRequestCheckpoint(producer, origin: serverOrigin.serialized)
        }
        return try vaultProgress(producer: producer, state: state)
    }

    /// The caller supplies the explicitly saved generated secret. Rust retains prepared material in the checkpoint,
    /// so retrying after a dropped response never silently creates a replacement secret.
    public func createVault(origin: String, username: String, adminPassword: String, passphrase: String) async throws -> SessionStatus {
        try await changeEnrollment {
            let serverOrigin = try ServerOrigin(canonical: origin, allowLoopbackHTTP: self.allowLoopbackHTTP)
            guard let producer = try self.restoredVaultRequestProducer(for: serverOrigin.serialized) else { throw ClientError.notEnrolled }
            let secrets = VaultSecretStore(secure: self.store.secure)
            let vaultSecret = try secrets.candidatePendingSecret(origin: serverOrigin.serialized, entered: passphrase)
            // The prepared owner material verifies the secret inside createVaultRequest, before any write or request.
            let request = try producer.createVaultRequest(username: username, adminPassword: adminPassword, passphrase: vaultSecret)
            try secrets.savePending(vaultSecret, origin: serverOrigin.serialized)
            try self.saveVaultRequestCheckpoint(producer, origin: serverOrigin.serialized)
            let host = EnrollmentHost(origin: serverOrigin, transport: self.transport, accountSession: try await self.hostedSession(for: serverOrigin))
            let response = try await host.createVault(request)
            let enrollment = try producer.finishOwnerAdmission(response, passphrase: vaultSecret)
            let status = try self.activateVerifiedEnrollment(enrollment, bindingSecret: vaultSecret)
            try self.clearVaultRequest(origin: serverOrigin.serialized, producer: producer)
            return status
        }
    }

    public func cancelVaultRequest(origin: String) throws {
        guard let producer = try restoredVaultRequestProducer(for: origin) else { return }
        guard try !producer.hasPreparedOwnerMaterial() else { return }
        try clearVaultRequest(origin: origin, producer: producer)
    }

    private func restoredVaultRequestProducer(for origin: String) throws -> NativeEnrollmentRequestProducer? {
        if vaultRequestOrigin == origin, let vaultRequestProducer { return vaultRequestProducer }
        guard let checkpoint = try store.secure.read(vaultRequestAccount(origin)) else { return nil }
        let producer = try NativeEnrollmentRequestProducer(checkpoint: checkpoint)
        guard let pending = try producer.pendingVaultRequest(), pending.origin == origin else {
            throw ClientError.invalidResponse("vault request checkpoint")
        }
        vaultRequestProducer = producer
        vaultRequestOrigin = origin
        return producer
    }

    private func saveVaultRequestCheckpoint(_ producer: NativeEnrollmentRequestProducer, origin: String) throws {
        try store.secure.upsert(vaultRequestAccount(origin), producer.checkpoint())
        vaultRequestProducer = producer
        vaultRequestOrigin = origin
    }

    private func clearVaultRequest(origin: String, producer: NativeEnrollmentRequestProducer) throws {
        try producer.clearVaultRequest()
        try store.secure.delete(vaultRequestAccount(origin))
        vaultRequestProducer = nil
        vaultRequestOrigin = nil
    }

    private func vaultProgress(producer: NativeEnrollmentRequestProducer, state: String) throws -> VaultRequestProgress {
        guard let pending = try producer.pendingVaultRequest() else { throw ClientError.notEnrolled }
        return VaultRequestProgress(
            origin: pending.origin,
            requestID: pending.requestId,
            displayName: pending.displayName,
            state: state,
            accepted: try producer.vaultRequestAccepted(),
            hasPreparedOwnerMaterial: try producer.hasPreparedOwnerMaterial(),
            hasStoredVaultSecret: try VaultSecretStore(secure: store.secure).pendingSecret(origin: pending.origin) != nil
        )
    }

    private func isDefinitiveInviteSubmissionFailure(_ error: Error) -> Bool {
        guard case let ClientError.server(status, _) = ClientError.wrap(error) else { return false }
        return (400..<500).contains(status) && status != 408 && status != 429
    }

    private func vaultRequestAccount(_ origin: String) -> String {
        let digest = SHA256.hash(data: Data(origin.utf8)).map { String(format: "%02x", $0) }.joined()
        return "vault-request:v1:\(digest)"
    }

    public func beginPhoneEnrollment(qrPayload: String, deviceName: String) async throws -> PhoneEnrollmentStatus {
        clearPhoneEnrollment()
        guard let payload = try? JSONSerialization.jsonObject(with: Data(qrPayload.utf8)) as? [String: Any],
              Set(payload.keys) == ["version", "purpose", "origin", "invitation_token", "transfer_public_key"],
              payload["version"] as? Int == 1, payload["purpose"] as? String == "connect_phone",
              let rawOrigin = payload["origin"] as? String else {
            throw ClientError.invalidCredential("phone enrollment QR")
        }
        let origin = try ServerOrigin(canonical: rawOrigin, allowLoopbackHTTP: allowLoopbackHTTP)
        let producer = try NativeEnrollmentRequestProducer(origin: origin.serialized, deviceName: deviceName)
        phoneProducer = producer
        do {
            let host = EnrollmentHost(origin: origin, transport: transport, accountSession: try await hostedSession(for: origin))
            try Task.checkCancellation()
            guard phoneProducer === producer else { throw ClientError.canceled }
            let request = try producer.claimPhoneInvitationRequest(qr: qrPayload)
            let claim = try await host.claimPhoneInvitation(request)
            try Task.checkCancellation()
            guard phoneProducer === producer else { throw ClientError.canceled }
            try producer.consumePhoneClaim(claim)
            let context = try phoneContext(from: claim, expectedOrigin: origin.serialized, host: host)
            phoneClaimContext = context
            return try await phoneEnrollmentStatus()
        } catch {
            producer.cancelPhoneFlow()
            clearPhoneEnrollment(cancelProducer: false, matching: producer)
            throw error
        }
    }

    public func phoneEnrollmentStatus() async throws -> PhoneEnrollmentStatus {
        guard let producer = phoneProducer, let context = phoneClaimContext else {
            throw ClientError.notEnrolled
        }
        let descriptor = try validatedPhoneStatusDescriptor(producer: producer, context: context)
        let status = try await context.host.executePhoneStatusDescriptor(descriptor)
        try Task.checkCancellation()
        guard phoneProducer === producer,
              phoneClaimContext?.invitationID == context.invitationID else {
            throw ClientError.canceled
        }
        let state: String
        do {
            guard let object = try JSONSerialization.jsonObject(with: Data(status.utf8)) as? [String: Any],
                  let parsedState = object["state"] as? String else {
                throw ClientError.invalidResponse("phone invitation")
            }
            if ["expired", "denied", "consumed"].contains(parsedState) {
                clearPhoneEnrollment()
                throw ClientError.server(status: 410, code: parsedState)
            }
            state = parsedState
        } catch {
            clearPhoneEnrollment(matching: producer)
            throw error
        }
        let code: String
        do {
            code = try producer.phoneSAS(status: status)
        } catch {
            clearPhoneEnrollment(matching: producer)
            throw error
        }
        return PhoneEnrollmentStatus(
            verificationCode: code,
            awaitingOwner: state != "approved",
            requiresSharedSecret: context.method == .sharedSecret
        )
    }

    public func confirmPhoneEnrollment(sharedSecret: String?) async throws -> SessionStatus {
        try await changeEnrollment {
            guard let producer = self.phoneProducer,
                  let context = self.phoneClaimContext else {
                throw ClientError.notEnrolled
            }
            let request: String
            if let pending = self.phoneConfirmationRequest {
                request = pending
            } else {
                let descriptor = try self.validatedPhoneStatusDescriptor(producer: producer, context: context)
                let status = try await context.host.executePhoneStatusDescriptor(descriptor)
                try Task.checkCancellation()
                guard self.phoneProducer === producer,
                      self.phoneClaimContext?.invitationID == context.invitationID else {
                    throw ClientError.canceled
                }
                do {
                    request = try producer.confirmPhoneInvitationRequest(status: status, sharedSecret: sharedSecret)
                    self.phoneConfirmationRequest = request
                } catch {
                    self.clearPhoneEnrollment(matching: producer)
                    throw error
                }
            }
            let response = try await context.host.confirmPhoneInvitation(id: context.invitationID, request: request)
            defer { self.clearPhoneEnrollment(cancelProducer: false, matching: producer) }
            try Task.checkCancellation()
            guard self.phoneProducer === producer,
                  self.phoneClaimContext?.invitationID == context.invitationID else {
                throw ClientError.canceled
            }
            let enrollment = try producer.finishedPhoneAdmission(response)
            return try self.activateVerifiedEnrollment(enrollment)
        }
    }

    public func cancelPhoneEnrollment() {
        clearPhoneEnrollment()
    }

    public func ownerPhoneApprovalHost(deviceName: String) async throws -> OwnerPhoneApprovalHost {
        guard let record, record.role == "owner", status.keysUnlocked else { throw ClientError.notEnrolled }
        let origin = try ServerOrigin(canonical: record.identity.origin, allowLoopbackHTTP: allowLoopbackHTTP)
        let client = try newNativeExistingOwnerEnrollmentClient(
            origin: origin.serialized,
            kind: .ios,
            deviceName: deviceName,
            profileJson: record.profileJson,
            headerJson: record.headerJson
        )
        let accountSession = try await hostedSession(for: origin)
        let host = EnrollmentHost(origin: origin, transport: transport, accountSession: accountSession)
        return OwnerPhoneApprovalHost(
            producer: NativeOwnerPhoneProducer(client: client),
            transport: EnrollmentOwnerPhoneTransport(host: host),
            secrets: VaultSecretStore(secure: store.secure),
            identity: record.identity,
            credential: try store.token(record.identity),
            isHosted: origin.serialized == "https://peppy.pro",
            accountSession: accountSession,
            shouldRotate: { stage, seconds, active, requested in
                let nativeStage: NativeOwnerPhoneStage
                switch stage {
                case .waiting: nativeStage = .waiting
                case .verify: nativeStage = .verify
                case .approved: nativeStage = .approved
                case .connected: nativeStage = .connected
                case .expired: nativeStage = .expired
                case .denied: nativeStage = .denied
                }
                return ownerPhoneShouldRotate(
                    stage: nativeStage,
                    expiresInSeconds: UInt32(clamping: seconds),
                    active: active == .transfer ? .transfer : .sharedSecret,
                    requested: requested == .transfer ? .transfer : .sharedSecret
                )
            }
        )
    }

    public func ownerVaultSecret() throws -> String {
        guard let record else { throw ClientError.notEnrolled }
        guard let secret = try VaultSecretStore(secure: store.secure).read(record.identity) else {
            throw ClientError.missingSecret("vault shared secret")
        }
        return secret
    }

    /// A network confirmation failure may retain Rust's prepared request for one deliberate retry.
    public func phoneEnrollmentCanRetryConfirmation() -> Bool {
        phoneProducer != nil && phoneClaimContext != nil && phoneConfirmationRequest != nil
    }

    private func phoneContext(
        from claim: String,
        expectedOrigin: String,
        host: EnrollmentHost
    ) throws -> PhoneClaimContext {
        guard let object = try JSONSerialization.jsonObject(with: Data(claim.utf8)) as? [String: Any],
              let invitationID = object["invitation_id"] as? String,
              UUID(uuidString: invitationID)?.uuidString.lowercased() == invitationID,
              object["origin"] as? String == expectedOrigin,
              let rawMethod = object["method"] as? String else {
            throw ClientError.invalidResponse("phone invitation claim")
        }
        let method: EnrollmentMethod
        switch rawMethod {
        case "transfer": method = .transfer
        case "shared_secret": method = .sharedSecret
        default: throw ClientError.invalidResponse("phone invitation claim")
        }
        return PhoneClaimContext(origin: expectedOrigin, invitationID: invitationID, method: method, host: host)
    }

    private func validatedPhoneStatusDescriptor(
        producer: NativeEnrollmentRequestProducer,
        context: PhoneClaimContext
    ) throws -> String {
        let descriptor = try producer.phoneStatusRequest()
        guard let object = try JSONSerialization.jsonObject(with: Data(descriptor.utf8)) as? [String: Any],
              object["method"] as? String == "GET",
              object["path"] as? String == "/v1/enrollment/invitations/\(context.invitationID)",
              context.origin == phoneClaimContext?.origin else {
            clearPhoneEnrollment()
            throw ClientError.invalidResponse("phone status descriptor")
        }
        return descriptor
    }

    private func clearPhoneEnrollment(cancelProducer: Bool = true, matching producer: NativeEnrollmentRequestProducer? = nil) {
        guard producer == nil || phoneProducer === producer else { return }
        if cancelProducer { phoneProducer?.cancelPhoneFlow() }
        phoneProducer = nil
        phoneClaimContext = nil
        phoneConfirmationRequest = nil
    }

    /// Revokes server access and deactivates the local enrollment. The encrypted database,
    /// keys and unacknowledged outbox remain archived, but the revoked credential cannot
    /// pass enrollment verification again. Rejoining requires pairing as a new device.
    public func disconnect() async throws -> SessionStatus {
        try await changeEnrollment {
            let storedIdentity = try self.store.activeIdentity()
            guard let active = self.record?.identity ?? storedIdentity else {
                self.close(); return self.status
            }
            let server = try self.authenticatedServer(active)
            var meter = RequestMeter(limit: 2)
            let response = try await server.exchange("POST", "/v1/devices/\(active.deviceId)/revoke", body: Data("{}".utf8), contentType: "application/json", limit: ServerClient.maxSmallBytes, meter: &meter)
            // A 401 for this identity is already proof its credential is unusable; archive it.
            guard (200..<300).contains(response.status) || response.status == 401 else { throw ClientError.server(status: response.status, code: nil) }
            guard try self.store.activeIdentity() == active else { throw ClientError.identityMismatch }
            self.close()
            try self.store.deactivate()
            return self.status
        }
    }

    /// Explicit offline escape hatch. It archives local state without revoking server access.
    /// Re-import can reopen that archive while the credential remains valid on the server.
    public func disconnectLocalOnly() async throws -> SessionStatus {
        try await changeEnrollment {
            let storedIdentity = try self.store.activeIdentity()
            let expected = self.record?.identity ?? storedIdentity
            guard expected == nil || storedIdentity == expected else { throw ClientError.identityMismatch }
            self.close()
            try self.store.deactivate()
            return self.status
        }
    }

    public func devices() async throws -> [DeviceRosterItem] {
        guard let record else { throw ClientError.notEnrolled }
        var meter = RequestMeter(limit: 2)
        let server = try authenticatedServer(record.identity)
        let response = try await server.exchange("GET", "/v1/devices", limit: ServerClient.maxSmallBytes, meter: &meter)
        let result = (try? JSONSerialization.jsonObject(with: response.body)) as? [String: Any]
        if response.status == 409, result?["code"] as? String == "resync_required" {
            throw ClientError.resyncRequired(reason: result?["reason"] as? String ?? "unknown")
        }
        guard (200..<300).contains(response.status) else {
            throw ClientError.server(status: response.status, code: result?["code"] as? String)
        }
        guard let result else { throw ClientError.invalidResponse("/v1/devices body") }
        return try result.objects("devices").map { item in
            let id = try item.string("device_id")
            guard UUID(uuidString: id) != nil,
                  let epoch = UInt32(exactly: try item.integer("key_epoch")) else { throw ClientError.invalidResponse("device roster") }
            return DeviceRosterItem(id: id, role: try item.string("role"), revoked: item["revoked"] as? Bool ?? false, keyEpoch: epoch)
        }
    }

    public func revokeDevice(_ deviceId: String) async throws -> SessionStatus {
        guard UUID(uuidString: deviceId) != nil else { throw ClientError.invalidCredential("device id") }
        return try await changeEnrollment {
            guard let record = self.record else { throw ClientError.notEnrolled }
            var meter = RequestMeter(limit: 2)
            let server = try self.authenticatedServer(record.identity)
            let response = try await server.exchange("POST", "/v1/devices/\(deviceId)/revoke", body: Data("{}".utf8), contentType: "application/json", limit: ServerClient.maxSmallBytes, meter: &meter)
            guard (200..<300).contains(response.status) || (deviceId == record.identity.deviceId && response.status == 401) else { throw ClientError.server(status: response.status, code: nil) }
            if deviceId == record.identity.deviceId {
                guard self.record?.identity == record.identity, try self.store.activeIdentity() == record.identity else { throw ClientError.identityMismatch }
                self.close(); try self.store.deactivate()
            }
            return self.status
        }
    }

    public func deleteVault(vaultId: String) async throws {
        guard let record else { throw ClientError.notEnrolled }
        var meter = RequestMeter(limit: 2)
        let server = try authenticatedServer(record.identity)
        let response = try await server.exchange("DELETE", "/v1/vault", body: try JSONSerialization.data(withJSONObject: ["vault_id": vaultId]), contentType: "application/json", limit: ServerClient.maxSmallBytes, meter: &meter)
        guard (200..<300).contains(response.status) else { throw ClientError.server(status: response.status, code: nil) }
        _ = try await disconnect()
    }

    /// Authenticated server publication deliberately receives only the route ID and wake
    /// credential; provider/manage credentials remain in the relay Keychain record.
    public func publishWakeRoute(routeId: String, wakeCredential: String) async throws {
        guard let record else { throw ClientError.notEnrolled }
        var meter = RequestMeter(limit: 2)
        let server = try authenticatedServer(record.identity)
        let response = try await server.exchange(
            "PUT", "/v1/devices/self/wake-route",
            body: try JSONSerialization.data(withJSONObject: ["route_id": routeId, "wake_credential": wakeCredential]),
            contentType: "application/json", limit: ServerClient.maxSmallBytes, meter: &meter
        )
        guard (200..<300).contains(response.status) else { throw ClientError.server(status: response.status, code: nil) }
    }

    private func changeEnrollment(_ body: () async throws -> SessionStatus) async throws -> SessionStatus {
        guard !changingEnrollment else { throw ClientError.enrollmentChangeInProgress }
        changingEnrollment = true
        defer { changingEnrollment = false }
        do { return try await body() } catch { throw ClientError.wrap(error) }
    }

    private func hostedSession(for origin: ServerOrigin) async throws -> String? {
        guard origin.serialized == "https://peppy.pro" else { return nil }
        return try await HostedAccountClient(transport: transport).enrollmentSessionBearer()
    }

    /// The identity may become (or stay) active: nothing else is active and its database, if one
    /// was ever created, still exists.
    private func requireAdoptable(_ identity: EnrollmentIdentity) throws {
        if let active = try store.activeIdentity(), active != identity { throw ClientError.alreadyEnrolled }
        if let open = record?.identity, open != identity { throw ClientError.alreadyEnrolled }
        if try store.databaseWasCreated(identity), !databaseExists(identity) { throw ClientError.localDatabaseMissing }
    }

    /// `bindingSecret` is set only by owner claim, vault creation, and manual admission, where the secret was just verified.
    private func activateVerifiedEnrollment(_ enrollment: NativeEnrollmentIdentity, bindingSecret: String? = nil) throws -> SessionStatus {
        let metadata = enrollment.metadata()
        let identity = EnrollmentIdentity(origin: metadata.origin, vaultId: metadata.vaultId, deviceId: metadata.deviceId)
        guard UUID(uuidString: identity.vaultId)?.uuidString.lowercased() == identity.vaultId,
              UUID(uuidString: identity.deviceId)?.uuidString.lowercased() == identity.deviceId,
              try ServerOrigin(canonical: identity.origin, allowLoopbackHTTP: allowLoopbackHTTP).serialized == identity.origin,
              let profile = try JSONSerialization.jsonObject(with: Data(metadata.profileJson.utf8)) as? [String: Any],
              let epoch = UInt32(exactly: profile["key_epoch"] as? Int ?? 0), epoch > 0
        else { throw ClientError.invalidResponse("verified enrollment metadata") }
        try requireAdoptable(identity)
        let nativeClient = try openClient(identity)
        do {
            try enrollment.activateInto(nativeClient: nativeClient)
            let credential = try enrollment.credentialForNativeSecureStore()
            let record = EnrollmentRecord(identity: identity, role: metadata.role, keyEpoch: epoch, profileJson: metadata.profileJson, headerJson: metadata.headerJson)
            try store.activate(record, token: credential)
            try store.saveKeyCache(nativeClient.exportNativeKeyCacheForNativeStorage(epoch: epoch), epoch: epoch, for: identity)
            client = nativeClient
            self.record = record
            status = SessionStatus(identity: identity, role: record.role, keyEpoch: epoch, databaseOpen: true, keysUnlocked: true)
            refreshCount()
            if let bindingSecret {
                VaultSecretStore(secure: store.secure).storeBinding(bindingSecret, for: identity, origin: identity.origin)
            }
            return status
        } catch {
            try? nativeClient.dispose()
            throw error
        }
    }

    /// Applies a verified record to the open client, or opens it.
    private func adopt(_ verified: EnrollmentRecord) throws -> SessionStatus {
        guard client != nil, record?.identity == verified.identity else { return try open() }
        let epochChanged = record?.keyEpoch != verified.keyEpoch
        record = verified
        status.role = verified.role
        status.keyEpoch = verified.keyEpoch
        if epochChanged {
            // Keys for a new epoch come only from its cache or from unlocking with the passphrase.
            status.keysUnlocked = false
            if let cache = try store.keyCache(verified.identity, epoch: verified.keyEpoch) {
                status.keysUnlocked = (try? client?.importNativeKeyCacheFromNativeStorage(bytes: cache)) != nil
            }
        }
        refreshCount()
        return status
    }

    // MARK: Database and keys

    /// Opens the active enrollment's database with its Keychain key and restores stored key caches.
    @discardableResult
    public func open() throws -> SessionStatus {
        do {
            guard let identity = try store.activeIdentity() else {
                close()
                return status
            }
            if let open = record?.identity, open != identity { throw ClientError.identityMismatch }
            let record = try store.record(identity)
            if client == nil {
                client = try openClient(identity)
                status = SessionStatus(identity: identity, databaseOpen: true)
                for (epoch, cache) in try store.keyCaches(identity, through: record.keyEpoch) {
                    do {
                        try client?.importNativeKeyCacheFromNativeStorage(bytes: cache)
                        if epoch == record.keyEpoch { status.keysUnlocked = true }
                    } catch {
                        status.rejectedKeyCaches += 1
                    }
                }
            }
            self.record = record
            status.role = record.role
            status.keyEpoch = record.keyEpoch
            refreshCount()
            return status
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// Unlocks with the vault's existing shared passphrase. The passphrase is passed to the core
    /// once and never stored; only the core's opaque key cache is kept in the Keychain.
    ///
    /// Only this manual unlock activates the server-verified epoch of the current record (a no-op
    /// when it is already active). Import, refresh and key-cache restoration never activate an epoch.
    public func unlock(passphrase: String) throws -> SessionStatus {
        guard let client, let record else { throw ClientError.notEnrolled }
        do {
            try client.unlock(profileJson: record.profileJson, headerJson: record.headerJson, passphrase: passphrase)
            try client.activateVerifiedEpoch(epoch: record.keyEpoch)
            let cache = try client.exportNativeKeyCacheForNativeStorage(epoch: record.keyEpoch)
            try store.saveKeyCache(cache, epoch: record.keyEpoch, for: record.identity)
        } catch {
            throw ClientError.wrap(error)
        }
        status.keysUnlocked = true
        refreshCount()
        return status
    }

    // MARK: Sync

    /// One bounded foreground pass. Cancel the calling task to stop it.
    public func syncOnce(budget: SyncBudget = SyncBudget()) async throws -> SyncReport {
        guard let client, let record else { throw ClientError.notEnrolled }
        guard !syncing else { throw ClientError.syncInProgress }
        syncing = true
        defer { syncing = false }
        let identity = record.identity
        do {
            let server = ServerClient(
                origin: try ServerOrigin(canonical: identity.origin, allowLoopbackHTTP: allowLoopbackHTTP),
                token: try store.token(identity),
                transport: transport
            )
            let report = try await ForegroundSync(client: client, server: server, vaultId: identity.vaultId, budget: budget).run()
            refreshCount()
            return report
        } catch {
            throw ClientError.wrap(error)
        }
    }

    // MARK: Contacts

    /// One bounded contact pass for foreground, change notifications and BackgroundTasks. It shares
    /// the sync guard, so it never overlaps a message sync. A background relaunch reopens the cached
    /// enrollment and key caches; locked keys (including a new epoch awaiting its passphrase) return
    /// `.needsUnlock` without capturing anything. Incoming records are applied before any permit.
    public func contactsPass(
        reason: ContactsPassReason,
        provider: any ContactsProvider,
        preferences: any ContactsPreferencesStore,
        budget: SyncBudget = SyncBudget(),
        isCancelled: @escaping @Sendable () -> Bool
    ) async throws -> ContactsSyncReport {
        guard preferences.load().enabled else { return ContactsSyncReport(outcome: .disabled) }
        if client == nil { _ = try open() }
        guard let client, let record else { throw ClientError.notEnrolled }
        guard status.keysUnlocked else { return ContactsSyncReport(outcome: .needsUnlock) }
        guard !syncing else { return ContactsSyncReport(outcome: .busy) }
        syncing = true
        defer { syncing = false }
        let identity = record.identity
        do {
            let server = ServerClient(
                origin: try ServerOrigin(canonical: identity.origin, allowLoopbackHTTP: allowLoopbackHTTP),
                token: try store.token(identity),
                transport: transport
            )
            let sync = ForegroundSync(client: client, server: server, vaultId: identity.vaultId, budget: budget)
            var syncError: ClientError?
            // Incoming records (requests, approvals, a repair snapshot) are applied before any permit.
            do {
                let pulled = try await sync.run()
                if pulled.contactRepairRequired, pulled.repairUnavailable { syncError = .unexpected("contact repair needs a compaction-capable server") }
            } catch {
                let wrapped = ClientError.wrap(error)
                if wrapped == .canceled { throw wrapped }
                syncError = wrapped // Offline is not fatal: capture stays local and uploads next time.
            }
            let scratch = databaseDirectory.appendingPathComponent("contact-media-tmp", isDirectory: true)
            try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
            try Self.protectUntilFirstUnlock(scratch)
            let media = syncError == nil ? ContactsMediaSession(
                transfer: ContactMediaTransfer(client: client, server: server, scratch: scratch),
                meter: RequestMeter(limit: budget.maxRequests)
            ) : nil
            var coordinator = NativeContactsCoordinator(
                client: client, deviceId: identity.deviceId, provider: provider, preferences: preferences
            )
            coordinator.scratch = scratch
            coordinator.media = media
            var report = try await coordinator.run(reason: reason, isCancelled: isCancelled)
            if let media, syncError == nil, !isCancelled(), !Task.isCancelled {
                // Ciphertext first, then references, so photo-bearing envelopes may publish.
                do {
                    try await media.transfer.uploadAndRegister(meter: &media.meter, report: &media.report)
                    try await media.transfer.reclaim(meter: &media.meter, report: &media.report)
                } catch {
                    let wrapped = ClientError.wrap(error)
                    if wrapped == .canceled { throw wrapped }
                    if wrapped != .budgetExhausted { syncError = wrapped }
                }
                report.media = media.report
            }
            if syncError == nil, !isCancelled(), !Task.isCancelled {
                do { _ = try await sync.run() } catch { syncError = ClientError.wrap(error) }
            }
            report.syncError = syncError?.userMessage
            refreshCount()
            return report
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// Owner book summary for settings: sanitized core state plus pending owner requests.
    public func contactsOverview() throws -> ContactsOverview {
        guard let client, let record else { throw ClientError.notEnrolled }
        do {
            let coordinator = NativeContactsCoordinator(
                client: client, deviceId: record.identity.deviceId, provider: NoContactsProvider(),
                preferences: InMemoryContactsPreferences()
            )
            return try coordinator.overview()
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// Owner policy for remote edits: `auto`, `confirm` or `off`. Stored and published by the core.
    public func setContactsRemoteEdits(_ mode: String) throws -> ContactsOverview {
        guard let client, let record else { throw ClientError.notEnrolled }
        do {
            let coordinator = NativeContactsCoordinator(
                client: client, deviceId: record.identity.deviceId, provider: NoContactsProvider(),
                preferences: InMemoryContactsPreferences()
            )
            try coordinator.setRemoteEdits(mode)
            return try coordinator.overview()
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// The account (OS container) new contacts are saved to; an owner setting stored by the core.
    public func setContactsDefaultAccount(_ accountId: String) throws -> ContactsOverview {
        guard let client, let record else { throw ClientError.notEnrolled }
        do {
            let coordinator = NativeContactsCoordinator(
                client: client, deviceId: record.identity.deviceId, provider: NoContactsProvider(),
                preferences: InMemoryContactsPreferences()
            )
            try coordinator.setDefaultAccount(accountId)
            return try coordinator.overview()
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// A local, explicit owner decision. Approval does not write: the next pass takes the permit.
    public func decideContactRequest(_ requestId: String, approve: Bool, scanHold: Bool = false) throws -> ContactsOverview {
        guard let client, let record else { throw ClientError.notEnrolled }
        do {
            let coordinator = NativeContactsCoordinator(
                client: client, deviceId: record.identity.deviceId, provider: NoContactsProvider(),
                preferences: InMemoryContactsPreferences()
            )
            try coordinator.decide(requestId, approve: approve, scanHold: scanHold)
            return try coordinator.overview()
        } catch {
            throw ClientError.wrap(error)
        }
    }

    /// Publishes the owned book as retired and turns syncing off. OS contacts are untouched; enabling
    /// again starts a new book.
    public func retireContacts(preferences: any ContactsPreferencesStore) throws -> ContactsOverview {
        guard let client, let record else { throw ClientError.notEnrolled }
        guard !syncing else { throw ClientError.syncInProgress }
        do {
            let coordinator = NativeContactsCoordinator(
                client: client, deviceId: record.identity.deviceId, provider: NoContactsProvider(),
                preferences: preferences
            )
            try coordinator.retire()
            var prefs = preferences.load()
            prefs.enabled = false
            preferences.save(prefs)
            return try coordinator.overview()
        } catch {
            throw ClientError.wrap(error)
        }
    }

    public func conversations() throws -> [NativeConversation] {
        guard let client else { throw ClientError.notEnrolled }
        do { return try client.listConversations() } catch { throw ClientError.wrap(error) }
    }

    private func authenticatedServer(_ identity: EnrollmentIdentity) throws -> ServerClient {
        try ServerClient(
            origin: ServerOrigin(canonical: identity.origin, allowLoopbackHTTP: allowLoopbackHTTP),
            token: store.token(identity), transport: transport
        )
    }

    /// Releases the client. Nothing on disk or in the Keychain changes.
    public func close() {
        try? client?.dispose()
        client = nil
        record = nil
        status = SessionStatus()
    }

    private func refreshCount() {
        status.conversations = try? client?.listConversations().count
    }

    private func databaseURL(_ identity: EnrollmentIdentity) -> URL {
        databaseDirectory.appendingPathComponent("\(identity.vaultId)-\(identity.deviceId).sqlcipher")
    }

    private func databaseExists(_ identity: EnrollmentIdentity) -> Bool {
        FileManager.default.fileExists(atPath: databaseURL(identity).path)
    }

    /// Background passes run while the device is locked after its first unlock, so the database,
    /// its sidecars and media must use `completeUntilFirstUserAuthentication`. New files inherit the
    /// directory's class; existing ones are set explicitly. A no-op off iOS.
    static func protectUntilFirstUnlock(_ directory: URL) throws {
        #if os(iOS)
        let attributes: [FileAttributeKey: Any] = [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication]
        let manager = FileManager.default
        try manager.setAttributes(attributes, ofItemAtPath: directory.path)
        guard let items = manager.enumerator(at: directory, includingPropertiesForKeys: nil) else { return }
        for case let item as URL in items {
            try manager.setAttributes(attributes, ofItemAtPath: item.path)
        }
        #endif
    }

    /// Fails closed when this identity's database was created before but is gone, or when a
    /// database file exists without its key; neither is ever replaced by a fresh database.
    private func openClient(_ identity: EnrollmentIdentity) throws -> NativeClient {
        let exists = databaseExists(identity)
        if try store.databaseWasCreated(identity), !exists { throw ClientError.localDatabaseMissing }
        var key = try store.databaseKey(identity, mayCreate: !exists)
        defer { key.resetBytes(in: 0..<key.count) }
        do {
            try FileManager.default.createDirectory(at: databaseDirectory, withIntermediateDirectories: true)
            var directory = databaseDirectory
            var values = URLResourceValues()
            // The database key is device-only, so a backed-up copy of the database could never be opened.
            values.isExcludedFromBackup = true
            try directory.setResourceValues(values)
            try Self.protectUntilFirstUnlock(databaseDirectory)
            let opened = try openNativeClient(config: NativeOpenConfig(
                databasePath: databaseURL(identity).path, vaultId: identity.vaultId, deviceId: identity.deviceId, databaseKey: key
            ))
            // SQLCipher creates its -wal/-shm sidecars and media directories on open.
            try Self.protectUntilFirstUnlock(databaseDirectory)
            try store.markDatabaseCreated(identity)
            return opened
        } catch {
            throw ClientError.wrap(error)
        }
    }
}

public struct SessionRelayWakeRoutePublisher: RelayWakeRoutePublisher {
    private let session: NativeSession
    public init(session: NativeSession) { self.session = session }
    public func publish(routeId: String, wakeCredential: String) async throws {
        try await session.publishWakeRoute(routeId: routeId, wakeCredential: wakeCredential)
    }
}

private extension UInt8 {
    var isASCIIBase64URL: Bool {
        (65...90).contains(self) || (97...122).contains(self) || (48...57).contains(self) || self == 45 || self == 95
    }

}
