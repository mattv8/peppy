import Foundation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

public enum OwnerPhoneMethod: Sendable, Equatable { case transfer, sharedSecret }
public enum OwnerPhoneStage: Sendable, Equatable { case waiting, verify, approved, connected, expired, denied }
public struct OwnerPhoneStatus: Sendable, Equatable {
    public let invitationId: String
    public let stage: OwnerPhoneStage
    public let expiresInSeconds: Int
    public let sas: String?
    public let claimDigest: String?

    public init(
        invitationId: String,
        stage: OwnerPhoneStage,
        expiresInSeconds: Int,
        sas: String?,
        claimDigest: String?
    ) {
        self.invitationId = invitationId
        self.stage = stage
        self.expiresInSeconds = expiresInSeconds
        self.sas = sas
        self.claimDigest = claimDigest
    }
}
/// Binding seam: Rust owns DTO parsing and supplies the invitation ID and typed status.
public protocol OwnerPhoneProducer: Sendable {
    func createInvitationRequest(method: OwnerPhoneMethod) throws -> String
    func consumeCreated(_ response: String) throws
    func invitationId() throws -> String
    func qrPayload() throws -> String
    func status(_ response: String) throws -> OwnerPhoneStatus
    func method() throws -> OwnerPhoneMethod?
    func secretMatches(_ secret: String) throws -> Bool
    func approveRequest(status: String, secret: String) throws -> String
}

public struct OwnerPhoneHTTPResponse: Sendable {
    public let status: Int
    public let body: String

    public init(status: Int, body: String) {
        self.status = status
        self.body = body
    }
}

/// Carries one device-authenticated owner request. `accountSession` is the hosted
/// `Peppy-Account-Session` value, or nil for self-hosted origins. The production adapter does not read it:
/// its `EnrollmentHost` already carries the same session (see `NativeSession.ownerPhoneApprovalHost`).
public protocol OwnerPhoneTransport: Sendable {
    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse?
}

public enum OwnerPhoneResult: Sendable, Equatable {
    case view(OwnerPhoneStatus, qr: String?)
    case cancelled
    case failed(String)
}

public actor OwnerPhoneApprovalHost {
    private let producer: any OwnerPhoneProducer
    private let transport: any OwnerPhoneTransport
    private let secrets: VaultSecretStore
    private let identity: EnrollmentIdentity
    private let credential: String
    private let isHosted: Bool
    private let accountSession: String?
    private let shouldRotate: @Sendable (OwnerPhoneStage, Int, OwnerPhoneMethod, OwnerPhoneMethod) -> Bool
    /// Set once an approval succeeds. The invitation then never reports Verify or Expired again.
    private var approvedInvitationId: String?

    public init(
        producer: any OwnerPhoneProducer,
        transport: any OwnerPhoneTransport,
        secrets: VaultSecretStore,
        identity: EnrollmentIdentity,
        credential: String,
        isHosted: Bool = false,
        accountSession: String? = nil,
        shouldRotate: @escaping @Sendable (OwnerPhoneStage, Int, OwnerPhoneMethod, OwnerPhoneMethod) -> Bool = { stage, seconds, active, requested in
            (stage == .waiting && (seconds <= 15 || active != requested)) || stage == .expired
        }
    ) {
        self.producer = producer
        self.transport = transport
        self.secrets = secrets
        self.identity = identity
        self.credential = credential
        self.isHosted = isHosted
        self.accountSession = accountSession
        self.shouldRotate = shouldRotate
    }

    public func start(method: OwnerPhoneMethod) async -> OwnerPhoneResult {
        guard authorized else { return .failed("account-required") }
        do {
            let response = try await send(
                "POST",
                "/v1/enrollment/invitations",
                try producer.createInvitationRequest(method: method)
            )
            guard let response else { return .failed("offline") }
            if let error = code(response.status) { return .failed(error) }
            try producer.consumeCreated(response.body)
            let created = OwnerPhoneStatus(
                invitationId: try producer.invitationId(),
                stage: .waiting,
                expiresInSeconds: 0,
                sas: nil,
                claimDigest: nil
            )
            return .view(created, qr: try producer.qrPayload())
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func poll() async -> OwnerPhoneResult {
        do {
            let status = try await current()
            let qr = status.stage == .waiting ? try producer.qrPayload() : nil
            return .view(status, qr: qr)
        } catch let failure as Failure {
            return .failed(failure.code)
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func rotateIfNeeded(requested: OwnerPhoneMethod) async -> OwnerPhoneResult {
        do {
            let status = try await current()
            let active = try producer.method() ?? requested
            guard shouldRotate(status.stage, status.expiresInSeconds, active, requested) else {
                let qr = status.stage == .waiting ? try producer.qrPayload() : nil
                return .view(status, qr: qr)
            }
            let response = try await send("DELETE", path(status.invitationId), nil)
            guard let response else { return .failed("offline") }
            guard response.status == 404 || response.status == 410 || code(response.status) == nil else {
                return .failed(code(response.status)!)
            }
            return await start(method: requested)
        } catch let failure as Failure {
            return .failed(failure.code)
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func approve(prompt: @escaping () async -> String?) async -> OwnerPhoneResult {
        do {
            let (status, json) = try await currentJSON()
            guard status.stage == .verify else { return .failed("enrollment-state") }
            if try producer.method() == .sharedSecret {
                return try await submit(status, json, "")
            }

            let saved: String?
            do {
                saved = try secrets.read(identity)
            } catch {
                return .failed("secure-store-unavailable")
            }
            if var secret = saved {
                defer { secret = "" }
                guard try producer.secretMatches(secret) else { return .failed("vault-secret-mismatch") }
                return try await submit(status, json, secret)
            }

            guard var secret = await prompt() else { return .cancelled }
            defer { secret = "" }
            let (fresh, freshJSON) = try await currentJSON()
            guard fresh.invitationId == status.invitationId, fresh.claimDigest == status.claimDigest else {
                return .cancelled
            }
            guard try producer.secretMatches(secret) else { return .failed("wrong-passphrase") }
            _ = secrets.saveIfMissing(secret, for: identity)
            return try await submit(fresh, freshJSON, secret)
        } catch let failure as Failure {
            return .failed(failure.code)
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func cancel() async -> OwnerPhoneResult {
        do {
            let status = try await current()
            guard status.stage == .waiting || status.stage == .expired else {
                return .failed("enrollment-state")
            }
            let response = try await send("DELETE", path(status.invitationId), nil)
            guard let response else { return .failed("offline") }
            if response.status == 404 || response.status == 410 { return .cancelled }
            guard let error = code(response.status) else { return .cancelled }
            return .failed(error)
        } catch let failure as Failure {
            return .failed(failure.code)
        } catch {
            return .failed(errorCode(error))
        }
    }

    private var authorized: Bool {
        !isHosted || accountSession != nil
    }

    private func current() async throws -> OwnerPhoneStatus {
        let (status, _) = try await currentJSON()
        return status
    }

    private func currentJSON() async throws -> (OwnerPhoneStatus, String) {
        guard authorized else { throw Failure("account-required") }
        let response = try await send("GET", path(try producer.invitationId()), nil)
        guard let response else { throw Failure("offline") }
        if let error = code(response.status) { throw Failure(error) }
        return (normalized(try producer.status(response.body)), response.body)
    }

    private func submit(_ status: OwnerPhoneStatus, _ json: String, _ secret: String) async throws -> OwnerPhoneResult {
        let request = try producer.approveRequest(status: json, secret: secret)
        let response = try await send("POST", "\(path(status.invitationId))/approve", request)
        guard let response else { return .failed("offline") }
        if let error = code(response.status) { return .failed(error) }
        approvedInvitationId = status.invitationId
        return .view(normalized(try producer.status(response.body)), qr: nil)
    }

    /// After approval, a stale Verify or a server-reported Expired is still the approved invitation.
    private func normalized(_ status: OwnerPhoneStatus) -> OwnerPhoneStatus {
        guard status.invitationId == approvedInvitationId else { return status }
        switch status.stage {
        case .verify, .expired, .approved:
            return OwnerPhoneStatus(
                invitationId: status.invitationId,
                stage: .approved,
                expiresInSeconds: status.expiresInSeconds,
                sas: nil,
                claimDigest: status.claimDigest
            )
        case .waiting, .connected, .denied:
            return status
        }
    }

    private func send(_ method: String, _ path: String, _ body: String?) async throws -> OwnerPhoneHTTPResponse? {
        try await transport.request(
            method: method,
            path: path,
            body: body,
            credential: credential,
            accountSession: accountSession
        )
    }

    private func path(_ id: String) -> String {
        "/v1/enrollment/invitations/\(id)"
    }

    private func code(_ status: Int) -> String? {
        guard !(200..<300).contains(status) else { return nil }
        switch status {
        case 404, 410: return "expired"
        case 409: return "enrollment-state"
        default: return "server-rejected"
        }
    }

    private func errorCode(_ error: any Error) -> String {
        if case ClientError.network = ClientError.wrap(error) { return "offline" }
        return "enrollment"
    }
}

private struct Failure: Error {
    let code: String

    init(_ code: String) {
        self.code = code
    }
}

/// Keeps generated owner enrollment types at the native boundary.
public final class NativeOwnerPhoneProducer: OwnerPhoneProducer, @unchecked Sendable {
    private let client: NativeEnrollmentClient

    public init(client: NativeEnrollmentClient) { self.client = client }

    public func createInvitationRequest(method: OwnerPhoneMethod) throws -> String {
        try client.createPhoneInvitationRequest(method: method == .transfer ? .transfer : .sharedSecret)
    }

    public func consumeCreated(_ response: String) throws { try client.consumeCreatedPhoneInvitation(createdJson: response) }

    public func invitationId() throws -> String {
        guard let value = try client.ownerPhoneInvitationId(), !value.isEmpty else {
            throw ClientError.invalidResponse("owner phone invitation")
        }
        return value
    }

    public func qrPayload() throws -> String { try client.ownerPhoneQrPayload() }

    public func status(_ response: String) throws -> OwnerPhoneStatus {
        let value = try client.ownerPhoneStatus(statusJson: response)
        let stage: OwnerPhoneStage
        switch value.stage {
        case .waiting: stage = .waiting
        case .verify: stage = .verify
        case .approved: stage = .approved
        case .connected: stage = .connected
        case .expired: stage = .expired
        case .denied: stage = .denied
        }
        return OwnerPhoneStatus(
            invitationId: value.invitationId,
            stage: stage,
            expiresInSeconds: Int(value.expiresInSeconds),
            sas: value.sas,
            claimDigest: value.claimDigest
        )
    }

    public func method() throws -> OwnerPhoneMethod? {
        switch try client.ownerPhoneMethod() {
        case .transfer: .transfer
        case .sharedSecret: .sharedSecret
        case nil: nil
        }
    }

    public func secretMatches(_ secret: String) throws -> Bool { try client.ownerPassphraseMatches(passphrase: secret) }

    public func approveRequest(status: String, secret: String) throws -> String {
        try client.approvePhoneInvitationRequest(statusJson: status, passphrase: secret)
    }
}

/// Production adapter. The hosted account session is fixed on the wrapped `EnrollmentHost` when `NativeSession`
/// builds it from the same `hostedSession(for:)` value, so the `accountSession` argument is not read here.
public struct EnrollmentOwnerPhoneTransport: OwnerPhoneTransport {
    private let host: EnrollmentHost

    public init(host: EnrollmentHost) { self.host = host }

    public func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        let segments = path.split(separator: "/")
        do {
            let response: String?
            switch (method, segments.last) {
            case ("POST", "invitations"):
                response = try await host.createPhoneInvitation(body ?? "", credential: credential)
            case ("GET", _):
                guard let invitationId = segments.last.map(String.init) else { throw ClientError.invalidResponse("owner phone path") }
                response = try await host.phoneInvitationStatus(id: invitationId, credential: credential)
            case ("POST", "approve"):
                guard let invitationId = segments.dropLast().last.map(String.init) else { throw ClientError.invalidResponse("owner phone path") }
                response = try await host.approvePhoneInvitation(id: invitationId, request: body ?? "", credential: credential)
            case ("DELETE", _):
                guard let invitationId = segments.last.map(String.init) else { throw ClientError.invalidResponse("owner phone path") }
                try await host.cancelPhoneInvitation(id: invitationId, credential: credential)
                response = "{}"
            default:
                throw ClientError.invalidResponse("owner phone request")
            }
            return OwnerPhoneHTTPResponse(status: 200, body: response ?? "{}")
        } catch let ClientError.server(status, _) {
            return OwnerPhoneHTTPResponse(status: status, body: "{}")
        }
    }
}
