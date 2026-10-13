import Foundation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

/// Native transport boundary for the Rust-owned enrollment state machine.
///
/// Swift carries only opaque JSON requests and responses. It neither derives
/// enrollment keys nor interprets handoff material.
public protocol EnrollmentRequestProducer: Sendable {
    func createOwnerRequest(setupToken: String, username: String, adminPassword: String, passphrase: String) throws -> String
    func manualChallengeRequest(vaultID: String?, vaultOwner: String?) throws -> String
    func completeManualChallengeRequest(challenge: String, passphrase: String) throws -> String
    func createPhoneInvitationRequest(method: EnrollmentMethod) throws -> String
    func consumeCreatedPhoneInvitation(_ response: String) throws
    func claimPhoneInvitationRequest(qr: String) throws -> String
    func consumePhoneClaim(_ response: String) throws
    func phoneSAS(status: String) throws -> String
    func approvePhoneInvitationRequest(status: String, passphrase: String) throws -> String
    func confirmPhoneInvitationRequest(status: String, sharedSecret: String?) throws -> String
}

public enum EnrollmentMethod: Sendable {
    case transfer
    case sharedSecret
}

/// Adapts the generated opaque Rust client without exposing its secrets to the
/// SwiftUI layer. All request JSON and every proof remain Rust-owned.
public final class NativeEnrollmentRequestProducer: EnrollmentRequestProducer, @unchecked Sendable {
    private let client: NativeEnrollmentClient

    public init(origin: String, deviceName: String) throws {
        client = try newNativeEnrollmentClient(origin: origin, kind: .ios, deviceName: deviceName)
    }

    init(checkpoint: Data) throws {
        client = try restoreNativeEnrollmentClient(checkpoint: checkpoint)
    }

    public func createOwnerRequest(setupToken: String, username: String, adminPassword: String, passphrase: String) throws -> String { try client.createOwnerRequest(setupToken: setupToken, username: username, adminPassword: adminPassword, passphrase: passphrase) }
    public func manualChallengeRequest(vaultID: String?, vaultOwner: String?) throws -> String { try client.manualChallengeRequest(vaultId: vaultID, vaultOwner: vaultOwner) }
    public func createOwnerRequest(setupToken: String, adminPassword: String, passphrase: String) throws -> String { try createOwnerRequest(setupToken: setupToken, username: "admin", adminPassword: adminPassword, passphrase: passphrase) }
    public func manualChallengeRequest(vaultID: String?) throws -> String { try manualChallengeRequest(vaultID: vaultID, vaultOwner: nil) }
    public func completeManualChallengeRequest(challenge: String, passphrase: String) throws -> String { try client.completeManualChallengeRequest(challengeJson: challenge, passphrase: passphrase) }
    public func createPhoneInvitationRequest(method: EnrollmentMethod) throws -> String { try client.createPhoneInvitationRequest(method: method == .transfer ? .transfer : .sharedSecret) }
    public func consumeCreatedPhoneInvitation(_ response: String) throws { try client.consumeCreatedPhoneInvitation(createdJson: response) }
    public func claimPhoneInvitationRequest(qr: String) throws -> String { try client.claimPhoneInvitationRequest(qrJson: qr) }
    public func consumePhoneClaim(_ response: String) throws { try client.consumePhoneClaim(claimJson: response) }
    public func phoneSAS(status: String) throws -> String { try client.phoneSas(statusJson: status) }
    public func approvePhoneInvitationRequest(status: String, passphrase: String) throws -> String { try client.approvePhoneInvitationRequest(statusJson: status, passphrase: passphrase) }
    public func confirmPhoneInvitationRequest(status: String, sharedSecret: String?) throws -> String { try client.confirmPhoneInvitationRequest(statusJson: status, sharedSecret: sharedSecret) }
    func finishedPhoneAdmission(_ response: String) throws -> NativeEnrollmentIdentity { try client.finishPhoneAdmission(completeJson: response) }

    func phoneStatusRequest() throws -> String { try client.phoneStatusRequest() }
    func cancelPhoneFlow() { try? client.cancelPhoneFlow() }

    func finishOwnerAdmission(_ response: String, passphrase: String) throws -> NativeEnrollmentIdentity {
        try client.finishOwnerAdmission(responseJson: response, passphrase: passphrase)
    }

    func finishManualAdmission(_ response: String, passphrase: String) throws -> NativeEnrollmentIdentity {
        try client.finishManualAdmission(responseJson: response, passphrase: passphrase)
    }

    func beginVaultRequest(origin: String, inviteToken: String, displayName: String) throws -> String { try client.beginVaultRequest(origin: origin, inviteToken: inviteToken, displayName: displayName) }
    func acceptVaultRequestCreated(_ response: String) throws { try client.acceptVaultRequestCreated(json: response) }
    func vaultRequestHeader() throws -> String { try client.vaultRequestHeader() }
    func parseVaultRequestStatus(_ response: String) throws -> String { try client.parseVaultRequestStatus(json: response) }
    func createVaultRequest(username: String, adminPassword: String, passphrase: String) throws -> String { try client.createVaultRequest(username: username, adminPassword: adminPassword, passphrase: passphrase) }
    func clearVaultRequest() throws { try client.clearVaultRequest() }
    func pendingVaultRequest() throws -> NativePendingVaultRequest? { try client.pendingVaultRequest() }
    func checkpoint() throws -> Data { try client.checkpoint() }
    func vaultRequestAccepted() throws -> Bool { try client.vaultRequestAccepted() }
    func hasPreparedOwnerMaterial() throws -> Bool { try client.hasPreparedOwnerMaterial() }
}

/// Performs only origin-bound JSON exchange for requests made by
/// `EnrollmentRequestProducer`. Credential installation is intentionally left
/// to the producer's native activation method.
public struct EnrollmentHost: Sendable {
    private let origin: ServerOrigin
    private let transport: any HTTPTransport
    private let accountSession: String?
    private let instanceSession: String?

    public init(origin: ServerOrigin, transport: any HTTPTransport, accountSession: String? = nil, instanceSession: String? = nil) {
        precondition(accountSession == nil || origin.serialized == "https://peppy.pro")
        self.origin = origin
        self.transport = transport
        self.accountSession = accountSession
        self.instanceSession = instanceSession
    }

    public func discoverInstance() async throws -> String {
        try await exchange(
            method: "GET",
            path: accountSession == nil ? "/v1/instance" : "/hosted/v1/enrollment/config",
            body: nil,
            headers: hostedHeaders()
        )
    }

    public func submitOwnerClaim(_ request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/instance/claim", body: request, headers: [:])
    }

    public func signIn(username: String, password: String) async throws -> String {
        let body = try JSONSerialization.data(withJSONObject: ["username": username, "admin_password": password, "client": "ios"])
        return try await exchange(method: "POST", path: "/v1/instance/session", body: String(decoding: body, as: UTF8.self), headers: [:])
    }

    public func instanceAccount() async throws -> String {
        try await exchange(method: "GET", path: "/v1/instance/account", body: nil, headers: instanceHeaders())
    }

    public func submitVaultRequest(_ request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/instance/vault-requests", body: request, headers: [:])
    }

    public func vaultRequestStatus(id: String, secret: String, knownState: String?) async throws -> String {
        var headers = ["Peppy-Vault-Request": secret]
        if let knownState { headers["Peppy-Known-State"] = knownState }
        return try await exchange(method: "GET", path: "/v1/instance/vault-requests/\(try enrollmentID(id))", query: [URLQueryItem(name: "wait", value: "15")], body: nil, headers: headers)
    }

    public func createVault(_ request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/instance/vaults", body: request, headers: [:])
    }

    public func requestManualChallenge(_ request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/challenges", body: request, headers: enrollmentHeaders())
    }

    public func completeManualChallenge(id: String, request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/challenges/\(try enrollmentID(id))/complete", body: request, headers: enrollmentHeaders())
    }

    public func createPhoneInvitation(_ request: String, credential: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/invitations", body: request, headers: ownerEnrollmentHeaders(credential))
    }

    public func claimPhoneInvitation(_ request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/invitations/claim", body: request, headers: hostedHeaders())
    }

    public func phoneInvitationStatus(id: String, credential: String? = nil, claimSecret: String? = nil) async throws -> String {
        var headers = credential.map(ownerEnrollmentHeaders) ?? [:]
        if let claimSecret { headers["Peppy-Claim-Secret"] = claimSecret }
        return try await exchange(method: "GET", path: "/v1/enrollment/invitations/\(try enrollmentID(id))", body: nil, headers: headers)
    }

    public func approvePhoneInvitation(id: String, request: String, credential: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/invitations/\(try enrollmentID(id))/approve", body: request, headers: ownerEnrollmentHeaders(credential))
    }

    public func confirmPhoneInvitation(id: String, request: String) async throws -> String {
        try await exchange(method: "POST", path: "/v1/enrollment/invitations/\(try enrollmentID(id))/confirm", body: request, headers: hostedHeaders())
    }

    public func cancelPhoneInvitation(id: String, credential: String) async throws {
        _ = try await exchange(method: "DELETE", path: "/v1/enrollment/invitations/\(try enrollmentID(id))", body: nil, headers: ownerEnrollmentHeaders(credential))
    }

    /// Runs a Rust-produced, claimant-authenticated descriptor. Its restricted
    /// grammar prevents the opaque claim secret from becoming a general HTTP
    /// capability or a UI-visible value.
    public func executePhoneStatusDescriptor(_ descriptor: String) async throws -> String {
        guard let value = try JSONSerialization.jsonObject(with: Data(descriptor.utf8)) as? [String: Any],
              Set(value.keys) == ["method", "path", "headers"],
              value["method"] as? String == "GET",
              let path = value["path"] as? String,
              let headers = value["headers"] as? [String: String],
              Set(headers.keys) == ["Peppy-Claim-Secret"],
              let secret = headers["Peppy-Claim-Secret"], !secret.isEmpty,
              path.hasPrefix("/v1/enrollment/invitations/"),
              !path.dropFirst("/v1/enrollment/invitations/".count).contains("/")
        else { throw ClientError.invalidResponse("phone status descriptor") }
        let id = String(path.dropFirst("/v1/enrollment/invitations/".count))
        return try await phoneInvitationStatus(id: id, claimSecret: secret)
    }

    private func exchange(method: String, path: String, query: [URLQueryItem] = [], body: String?, headers: [String: String]) async throws -> String {
        var requestHeaders = headers
        requestHeaders["Accept"] = "application/json"
        if body != nil { requestHeaders["Content-Type"] = "application/json" }
        let response = try await transport.send(HTTPRequest(
            method: method,
            url: origin.url(path, query: query),
            headers: requestHeaders,
            body: body.map { Data($0.utf8) },
            maxResponseBytes: ServerClient.maxSmallBytes
        ))
        guard origin.contains(response.url) else { throw ClientError.originMismatch }
        guard (200..<300).contains(response.status) else {
            let code = (try? JSONSerialization.jsonObject(with: response.body) as? [String: Any])?["code"] as? String
            throw ClientError.server(status: response.status, code: code)
        }
        guard let json = String(data: response.body, encoding: .utf8),
              (try? JSONSerialization.jsonObject(with: response.body)) != nil else {
            throw ClientError.invalidResponse(path)
        }
        return json
    }

    private func enrollmentID(_ value: String) throws -> String {
        guard UUID(uuidString: value)?.uuidString.lowercased() == value else {
            throw ClientError.invalidCredential("enrollment id")
        }
        return value
    }

    private func authorization(_ credential: String) -> [String: String] {
        ["Authorization": "Bearer \(credential)"]
    }

    private func ownerEnrollmentHeaders(_ credential: String) -> [String: String] {
        var headers = authorization(credential)
        headers.merge(hostedHeaders()) { _, account in account }
        return headers
    }

    private func hostedHeaders() -> [String: String] {
        accountSession.map { ["Peppy-Account-Session": $0] } ?? [:]
    }

    private func instanceHeaders() -> [String: String] {
        instanceSession.map { ["Peppy-Instance-Session": $0] } ?? [:]
    }

    private func enrollmentHeaders() -> [String: String] {
        var headers = hostedHeaders()
        headers.merge(instanceHeaders()) { _, instance in instance }
        return headers
    }
}
