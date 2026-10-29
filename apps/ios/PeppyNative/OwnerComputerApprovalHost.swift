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
    private var pendingInvitationID: String?
    private var operationTail: Task<Void, Never>?

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
        await serialized { await self.scanRequest(qr: qr) }
    }

    private func scanRequest(qr: String) async -> OwnerComputerResult {
        guard authorized else { return .failed("account-required") }
        var createdPrompt = false
        do {
            let prompt = try producer.createRequest(qr: qr)
            createdPrompt = true
            guard prompt.origin == origin else {
                return failAndClear("invalid-computer-qr")
            }
            pendingInvitationID = prompt.invitationID
            let response = try await send("POST", "/v1/enrollment/computers", prompt.requestJSON)
            guard let response else { return failAndClear("offline") }
            if let error = code(response.status) { return failAndClear(error) }
            try producer.consumeCreated(invitationID: prompt.invitationID, promptDigest: prompt.promptDigest, response: response.body)
            guard let view = try approval(), view.invitationID == prompt.invitationID, view.promptDigest == prompt.promptDigest else {
                return failAndClear("enrollment-state")
            }
            return .view(view)
        } catch {
            return createdPrompt ? failAndClear(errorCode(error)) : .failed(errorCode(error))
        }
    }

    public func approve(codesMatch: Bool, prompt: @escaping @Sendable () async -> String?) async -> OwnerComputerResult {
        await serialized { await self.approveRequest(codesMatch: codesMatch, prompt: prompt) }
    }

    private func approveRequest(codesMatch: Bool, prompt: @escaping @Sendable () async -> String?) async -> OwnerComputerResult {
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
            approvedPrompt = fresh
            guard let current = try approval(), current.invitationID == fresh.invitationID, current.promptDigest == fresh.promptDigest else {
                return .cancelled
            }
            return .approved
        } catch {
            return .failed(errorCode(error))
        }
    }

    public func cancel() async -> OwnerComputerResult {
        await serialized { await self.cancelRequest() }
    }

    private func cancelRequest() async -> OwnerComputerResult {
        if approvedPrompt != nil { return .cancelled }
        let invitationID = ((try? approval()) ?? nil)?.invitationID ?? pendingInvitationID
        clear()
        pendingInvitationID = nil
        if let invitationID { _ = try? await send("DELETE", path(invitationID), nil) }
        return .cancelled
    }

    private func clear() {
        try? producer.cancel()
    }

    private func failAndClear(_ error: String) -> OwnerComputerResult {
        clear()
        pendingInvitationID = nil
        return .failed(error)
    }

    public func dismiss() async {
        _ = await serialized { await self.cancelRequest() }
    }

    private func serialized<Result: Sendable>(_ operation: @escaping @Sendable () async -> Result) async -> Result {
        let previous = operationTail
        let task = Task { [previous] in
            await previous?.value
            return await operation()
        }
        operationTail = Task { _ = await task.value }
        return await task.value
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
