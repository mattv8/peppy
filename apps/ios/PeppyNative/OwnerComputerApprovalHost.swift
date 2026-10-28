import Foundation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

public struct OwnerComputerPrompt: Sendable, Equatable {
    public let origin: String
    public let invitationID: String
    public let promptDigest: String
    public let requestJSON: String
}

public struct OwnerComputerApproval: Sendable, Equatable {
    public let invitationID: String
    public let promptDigest: String
    public let sas: String
}

public protocol OwnerComputerProducer: Sendable {
    func createRequest(qr: String) throws -> OwnerComputerPrompt
    func consumeCreated(invitationID: String, promptDigest: String, response: String) throws
    func view() throws -> OwnerComputerApproval?
    func approve(invitationID: String, promptDigest: String, codesMatch: Bool, vaultKeyPhrase: String) throws -> String
    func cancel() throws
}

public protocol OwnerComputerTransport: Sendable {
    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse?
}

public enum OwnerComputerResult: Sendable, Equatable {
    case view(OwnerComputerApproval)
    case approved
    case cancelled
    case failed(String)
}

/// Coordinates a scanned, Rust-validated computer QR with the authenticated owner transport.
/// The QR origin is checked against the protected enrolled identity before any request is made.
public actor OwnerComputerApprovalHost {
    private let producer: any OwnerComputerProducer
    private let transport: any OwnerComputerTransport
    private let secrets: VaultSecretStore
    private let identity: EnrollmentIdentity
    private let credential: String
    private let origin: String
    private let isHosted: Bool
    private let accountSession: String?
    private var approvedPrompt: OwnerComputerApproval?
    private var localFlowCleared = false

    public init(
        producer: any OwnerComputerProducer,
        transport: any OwnerComputerTransport,
        secrets: VaultSecretStore,
        identity: EnrollmentIdentity,
        credential: String,
        isHosted: Bool = false,
        accountSession: String? = nil
    ) {
        self.producer = producer
        self.transport = transport
        self.secrets = secrets
        self.identity = identity
        self.credential = credential
        origin = identity.origin
        self.isHosted = isHosted
        self.accountSession = accountSession
    }

    public func scan(qr: String) async -> OwnerComputerResult {
        guard authorized else { return .failed("account-required") }
        do {
            let prompt = try producer.createRequest(qr: qr)
            guard prompt.origin == origin else {
                try producer.cancel()
                return .failed("invalid-computer-qr")
            }
            let response = try await send("POST", "/v1/enrollment/computers", prompt.requestJSON)
            guard let response else { return .failed("offline") }
            if let error = code(response.status) { return .failed(error) }
            try producer.consumeCreated(invitationID: prompt.invitationID, promptDigest: prompt.promptDigest, response: response.body)
            guard let view = try approval(), view.invitationID == prompt.invitationID, view.promptDigest == prompt.promptDigest else {
                return .failed("enrollment-state")
            }
            return .view(view)
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func approve(codesMatch: Bool, prompt: @escaping () async -> String?) async -> OwnerComputerResult {
        guard codesMatch else { return .failed("codes-do-not-match") }
        do {
            guard let initial = try approval() else { return .failed("enrollment-state") }
            var phrase: String
            do {
                if let stored = try secrets.read(identity) {
                    phrase = stored
                } else {
                    guard let entered = await prompt() else { return .cancelled }
                    phrase = entered
                }
            } catch {
                return .failed("secure-store-unavailable")
            }
            defer { phrase = "" }
            guard let fresh = try approval(), fresh.invitationID == initial.invitationID, fresh.promptDigest == initial.promptDigest else {
                return .cancelled
            }
            let request = try producer.approve(
                invitationID: fresh.invitationID,
                promptDigest: fresh.promptDigest,
                codesMatch: true,
                vaultKeyPhrase: phrase
            )
            // The binding verifies the phrase against the protected profile before this write.
            _ = secrets.saveIfMissing(phrase, for: identity)
            let response = try await send("POST", path(fresh.invitationID, "approve"), request)
            guard let response else { return .failed("offline") }
            if let error = code(response.status) { return .failed(error) }
            guard let current = try approval(), current.invitationID == fresh.invitationID, current.promptDigest == fresh.promptDigest else {
                return .cancelled
            }
            approvedPrompt = fresh
            return .approved
        } catch {
            return .failed(errorCode(error))
        }
    }

    /// Cancellation always clears the local QR secret and pending handoff state, including network failures.
    public func cancel() async -> OwnerComputerResult {
        if approvedPrompt != nil { return .cancelled }
        defer { try? producer.cancel() }
        do {
            guard let active = try approval() else { return .cancelled }
            let response = try await send("DELETE", path(active.invitationID), nil)
            guard let response else { return .failed("offline") }
            if response.status == 404 || response.status == 410 { return .cancelled }
            if let error = code(response.status) { return .failed(error) }
            return .cancelled
        } catch {
            return .failed(errorCode(error))
        }
    }

    /// Clears Rust's local prompt, nonce and cached approval body without changing server state.
    private func clear() {
        guard !localFlowCleared else { return }
        localFlowCleared = true
        try? producer.cancel()
    }

    /// Closing an approved surface must not revoke the server enrollment or discard its local transcript.
    public func dismiss() {
        guard approvedPrompt == nil else { return }
        clear()
    }

    private var authorized: Bool { !isHosted || accountSession != nil }

    private func approval() throws -> OwnerComputerApproval? {
        try producer.view()
    }

    private func send(_ method: String, _ path: String, _ body: String?) async throws -> OwnerPhoneHTTPResponse? {
        try await transport.request(method: method, path: path, body: body, credential: credential, accountSession: accountSession)
    }

    private func path(_ invitationID: String, _ suffix: String = "") -> String {
        "/v1/enrollment/computers/\(invitationID)\(suffix.isEmpty ? "" : "/\(suffix)")"
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

public final class NativeOwnerComputerProducer: OwnerComputerProducer, @unchecked Sendable {
    private let client: NativeEnrollmentClient
    private let nativeClient: NativeClient

    public init(client: NativeEnrollmentClient, nativeClient: NativeClient) {
        self.client = client
        self.nativeClient = nativeClient
    }

    public func createRequest(qr: String) throws -> OwnerComputerPrompt {
        let request = try client.ownerComputerCreateRequest(qrJson: qr)
        return OwnerComputerPrompt(origin: request.origin, invitationID: request.invitationId, promptDigest: request.promptDigest, requestJSON: request.requestJson)
    }

    public func consumeCreated(invitationID: String, promptDigest: String, response: String) throws {
        try client.consumeOwnerComputerCreated(invitationId: invitationID, promptDigest: promptDigest, createdJson: response)
    }

    public func view() throws -> OwnerComputerApproval? {
        let view = try client.ownerComputerView()
        guard view.created, let sas = view.sas else { return nil }
        return OwnerComputerApproval(invitationID: view.invitationId, promptDigest: view.promptDigest, sas: sas)
    }

    public func approve(invitationID: String, promptDigest: String, codesMatch: Bool, vaultKeyPhrase: String) throws -> String {
        try client.approveOwnerComputerRequest(invitationId: invitationID, promptDigest: promptDigest, codesMatch: codesMatch, vaultKeyPhrase: vaultKeyPhrase, nativeClient: nativeClient)
    }

    public func cancel() throws { try client.cancelOwnerComputerFlow() }
}

public struct EnrollmentOwnerComputerTransport: OwnerComputerTransport {
    private let host: EnrollmentHost

    public init(host: EnrollmentHost) { self.host = host }

    public func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        do {
            let segments = path.split(separator: "/")
            let response: String?
            switch (method, segments.last) {
            case ("POST", "computers"):
                response = try await host.createComputerEnrollment(body ?? "", credential: credential)
            case ("POST", "approve"):
                guard let id = segments.dropLast().last.map(String.init) else { throw ClientError.invalidResponse("owner computer path") }
                response = try await host.approveComputerEnrollment(id: id, request: body ?? "", credential: credential)
            case ("DELETE", _):
                guard let id = segments.last.map(String.init) else { throw ClientError.invalidResponse("owner computer path") }
                try await host.cancelComputerEnrollment(id: id, credential: credential)
                response = "{}"
            default:
                throw ClientError.invalidResponse("owner computer request")
            }
            return OwnerPhoneHTTPResponse(status: 200, body: response ?? "{}")
        } catch let ClientError.server(status, _) {
            return OwnerPhoneHTTPResponse(status: status, body: "{}")
        }
    }
}
