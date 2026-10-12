import Foundation
import Testing
@testable import PeppyNative

@Suite struct EnrollmentHostTests {
    @Test func sendsOpaqueJSONToTheEnrollmentRoute() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://enroll.example.test", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport)

        _ = try await host.submitOwnerClaim("{\"operation_id\":\"request-owned-by-rust\"}")

        let request = try #require(transport.request)
        #expect(request.method == "POST")
        #expect(request.url.path == "/v1/instance/claim")
        #expect(request.headers["Content-Type"] == "application/json")
        #expect(request.body == Data("{\"operation_id\":\"request-owned-by-rust\"}".utf8))
    }

    @Test func vaultRequestPollingUsesTheProtectedHeaderAndBoundedWait() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://enroll.example.test", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport)
        let id = "4a440c5f-8cae-4e4b-aae2-7472c64a08a1"

        _ = try await host.vaultRequestStatus(id: id, secret: "opaque-request-secret", knownState: "pending")

        let request = try #require(transport.request)
        #expect(request.url.path == "/v1/instance/vault-requests/\(id)")
        #expect(URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems == [URLQueryItem(name: "wait", value: "15")])
        #expect(request.headers["Peppy-Vault-Request"] == "opaque-request-secret")
        #expect(request.headers["Peppy-Known-State"] == "pending")
    }

    @Test func restartResubmitsTheProtectedUnacceptedRequestBeforePolling() async throws {
        let transport = RecoveringVaultRequestTransport()
        let store = InMemorySecureStore()
        let token = Data(repeating: 0, count: 32).base64EncodedString().replacingOccurrences(of: "=", with: "")
        let link = "https://enroll.example.test/#invite=\(token)"
        let first = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)

        await #expect(throws: URLError(.notConnectedToInternet)) {
            _ = try await first.beginVaultRequest(inviteLink: link, displayName: "Taylor", expectedOrigin: nil, deviceName: "iPhone")
        }

        let restored = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        let progress = try await restored.vaultRequestStatus(origin: "https://enroll.example.test")

        #expect(progress.state == "pending")
        #expect(transport.submissions == 2)
        #expect(transport.statusRequests == 1)
    }

    @Test func rejectsAnInvitationIdentifierThatIsNotCanonicalUUID() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://enroll.example.test", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport)

        await #expect(throws: ClientError.invalidCredential("enrollment id")) {
            _ = try await host.confirmPhoneInvitation(id: "not-an-id", request: "{}")
        }
        #expect(transport.request == nil)
    }

    @Test func pinsHostedAccountSessionToHostedConfiguration() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://peppy.pro", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport, accountSession: "native-session")

        _ = try await host.discoverInstance()

        let request = try #require(transport.request)
        #expect(request.url.path == "/hosted/v1/enrollment/config")
        #expect(request.headers["Peppy-Account-Session"] == "native-session")
        #expect(request.headers["Authorization"] == nil)
    }

    @Test func discoveryUsesExplicitOpenPolicyOnly() async throws {
        let transport = EnrollmentTransport(body: #"{"mode":"community","claimed":false,"setup_required":false}"#)
        let session = NativeSession(
            secureStore: InMemorySecureStore(),
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )

        let discovery = try await session.discoverSelfHostedInstance(origin: "https://enroll.example.test")

        #expect(discovery.origin == "https://enroll.example.test")
        #expect(!discovery.setupCodeRequired)
    }

    @Test func discoveryWithoutPolicyRemainsProtected() async throws {
        let transport = EnrollmentTransport(body: #"{"mode":"community","claimed":false}"#)
        let session = NativeSession(
            secureStore: InMemorySecureStore(),
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )

        let discovery = try await session.discoverSelfHostedInstance(origin: "https://enroll.example.test")

        #expect(discovery.setupCodeRequired)
    }

    @Test func malformedDiscoveryCannotAuthorizeOpenSetup() async throws {
        let transport = EnrollmentTransport(body: "[]")
        let session = NativeSession(
            secureStore: InMemorySecureStore(),
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )

        await #expect(throws: ClientError.invalidResponse("instance discovery")) {
            _ = try await session.discoverSelfHostedInstance(origin: "https://enroll.example.test")
        }
    }

    @Test(arguments: [
        #"{"mode":"community","claimed":0,"setup_required":false}"#,
        #"{"mode":"community","setup_required":false}"#,
        #"{"mode":"hosted","claimed":false,"setup_required":false}"#,
    ])
    func discoveryRejectsMalformedCommunityPolicy(_ body: String) async throws {
        let transport = EnrollmentTransport(body: body)
        let session = NativeSession(
            secureStore: InMemorySecureStore(),
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )

        await #expect(throws: ClientError.invalidResponse("instance discovery")) {
            _ = try await session.discoverSelfHostedInstance(origin: "https://enroll.example.test")
        }
    }

    @Test func claimedDiscoveryReturnsTheExistingAdmissionChooser() async throws {
        let transport = EnrollmentTransport(body: #"{"mode":"community","claimed":true,"setup_required":false}"#)
        let session = NativeSession(
            secureStore: InMemorySecureStore(),
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )

        let discovery = try await session.discoverSelfHostedInstance(origin: "https://enroll.example.test")
        #expect(discovery.claimed)
    }

    @Test func executesOnlyAValidClaimantStatusDescriptor() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://enroll.example.test", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport)
        let id = "4a440c5f-8cae-4e4b-aae2-7472c64a08a1"
        let descriptor = "{\"method\":\"GET\",\"path\":\"/v1/enrollment/invitations/\(id)\",\"headers\":{\"Peppy-Claim-Secret\":\"opaque\"}}"

        _ = try await host.executePhoneStatusDescriptor(descriptor)

        let request = try #require(transport.request)
        #expect(request.url.path == "/v1/enrollment/invitations/\(id)")
        #expect(request.headers["Peppy-Claim-Secret"] == "opaque")
        #expect(request.headers["Authorization"] == nil)
    }
}

private final class EnrollmentTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var latest: HTTPRequest?
    private let body: Data

    init(body: String = "{}") {
        self.body = Data(body.utf8)
    }

    var request: HTTPRequest? { lock.withLock { latest } }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { latest = request }
        return HTTPResponse(status: 200, url: request.url, body: body)
    }
}

private final class RecoveringVaultRequestTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var requestID = ""
    private var firstSubmission = true
    private(set) var submissions = 0
    private(set) var statusRequests = 0

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        try lock.withLock {
            switch (request.method, request.url.path) {
            case ("POST", "/v1/instance/vault-requests"):
                submissions += 1
                let body = try #require(request.body)
                requestID = try #require((JSONSerialization.jsonObject(with: body) as? [String: Any])?["request_id"] as? String)
                if firstSubmission {
                    firstSubmission = false
                    throw URLError(.notConnectedToInternet)
                }
                return response(request, ["request_id": requestID, "expires_in_seconds": 60])
            case ("GET", "/v1/instance/vault-requests/\(requestID)"):
                statusRequests += 1
                return response(request, ["request_id": requestID, "state": "pending", "expires_in_seconds": 60])
            default:
                return HTTPResponse(status: 404, url: request.url, body: Data("{\"code\":\"request_not_found\"}".utf8))
            }
        }
    }

    private func response(_ request: HTTPRequest, _ body: [String: Any]) -> HTTPResponse {
        HTTPResponse(status: 200, url: request.url, body: try! JSONSerialization.data(withJSONObject: body))
    }
}

extension EnrollmentHostTests {
    @Test func transferMethodComesFromVerifiedClaimWhenStatusOmitsContextFields() async throws {
        let transport = try PhoneEnrollmentTransport(method: "transfer")
        let session = makePhoneSession(transport: transport)

        let status = try await session.beginPhoneEnrollment(qrPayload: transport.qrPayload, deviceName: "iPhone")

        #expect(!status.requiresSharedSecret)
        #expect(!transport.statusContainsMethodOrInvitationID)
    }

    @Test func sharedSecretMethodAndInvitationIDSurviveStatusWithoutContextFields() async throws {
        let transport = try PhoneEnrollmentTransport(method: "shared_secret")
        let session = makePhoneSession(transport: transport)

        let status = try await session.beginPhoneEnrollment(qrPayload: transport.qrPayload, deviceName: "iPhone")
        #expect(status.requiresSharedSecret)
        #expect(!transport.statusContainsMethodOrInvitationID)

        await #expect(throws: (any Error).self) {
            _ = try await session.confirmPhoneEnrollment(sharedSecret: PhoneEnrollmentTransport.passphrase)
        }

        #expect(transport.paths.contains("POST /v1/enrollment/invitations/\(transport.invitationID)/confirm"))
        #expect(transport.confirmationRequestHasProofs)
        await #expect(throws: ClientError.notEnrolled) {
            _ = try await session.phoneEnrollmentStatus()
        }
    }

    @Test func cancellationClearsTheVerifiedClaimContext() async throws {
        let transport = try PhoneEnrollmentTransport(method: "transfer")
        let session = makePhoneSession(transport: transport)
        _ = try await session.beginPhoneEnrollment(qrPayload: transport.qrPayload, deviceName: "iPhone")

        await session.cancelPhoneEnrollment()

        await #expect(throws: ClientError.notEnrolled) {
            _ = try await session.phoneEnrollmentStatus()
        }
    }

    @Test func confirmationNetworkFailureRetriesThePreparedRustRequest() async throws {
        let transport = try PhoneEnrollmentTransport(method: "shared_secret", confirmationNetworkFailures: 1)
        let session = makePhoneSession(transport: transport)
        _ = try await session.beginPhoneEnrollment(qrPayload: transport.qrPayload, deviceName: "iPhone")

        await #expect(throws: ClientError.server(status: 503, code: "temporarily_unavailable")) {
            _ = try await session.confirmPhoneEnrollment(sharedSecret: PhoneEnrollmentTransport.passphrase)
        }
        await #expect(throws: (any Error).self) {
            _ = try await session.confirmPhoneEnrollment(sharedSecret: PhoneEnrollmentTransport.passphrase)
        }

        #expect(transport.confirmationAttempts == 2)
    }

    @Test func malformedClaimNeverActivatesEnrollment() async throws {
        let transport = try PhoneEnrollmentTransport(method: "shared_secret", malformedClaim: true)
        let store = InMemorySecureStore()
        let session = makePhoneSession(transport: transport, store: store)

        await #expect(throws: (any Error).self) {
            _ = try await session.beginPhoneEnrollment(qrPayload: transport.qrPayload, deviceName: "iPhone")
        }

        #expect(store.accounts.isEmpty)
        #expect((try await session.open()).identity == nil)
    }

    @Test func rejectsForeignPhoneStatusDescriptorWithoutSendingRequest() async throws {
        let transport = EnrollmentTransport()
        let origin = try ServerOrigin(canonical: "https://enroll.example.test", allowLoopbackHTTP: false)
        let host = EnrollmentHost(origin: origin, transport: transport)
        let descriptor = "{\"method\":\"GET\",\"path\":\"https://other.example.test/v1/enrollment/invitations/4a440c5f-8cae-4e4b-aae2-7472c64a08a1\",\"headers\":{\"Peppy-Claim-Secret\":\"opaque\"}}"

        await #expect(throws: ClientError.invalidResponse("phone status descriptor")) {
            _ = try await host.executePhoneStatusDescriptor(descriptor)
        }
        #expect(transport.request == nil)
    }

    private func makePhoneSession(
        transport: PhoneEnrollmentTransport,
        store: InMemorySecureStore = InMemorySecureStore()
    ) -> NativeSession {
        NativeSession(
            secureStore: store,
            transport: transport,
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )
    }
}

private final class PhoneEnrollmentTransport: HTTPTransport, @unchecked Sendable {
    static let origin = "https://phone-enrollment.example.test"
    static let passphrase = "phone enrollment shared secret"

    let invitationID = "4a440c5f-8cae-4e4b-aae2-7472c64a08a1"
    let qrPayload: String
    let vaultID: String
    let method: String

    private let malformedClaim: Bool
    private let ownerTransferPublicKey: String
    private let material: [String: Any]
    private let lock = NSLock()
    private var recordedRequests: [HTTPRequest] = []
    private var claimRequest: [String: Any]?
    private var latestStatus: [String: Any]?
    private var remainingConfirmationFailures: Int

    var paths: [String] {
        lock.withLock { recordedRequests.map { "\($0.method) \($0.url.path)" } }
    }

    init(
        method: String,
        malformedClaim: Bool = false,
        confirmationNetworkFailures: Int = 0
    ) throws {
        self.method = method
        self.malformedClaim = malformedClaim
        remainingConfirmationFailures = confirmationNetworkFailures
        let owner = try NativeEnrollmentRequestProducer(origin: Self.origin, deviceName: "fixture owner")
        let ownerRequest = try Self.object(owner.createOwnerRequest(
            setupToken: "operator-code",
            adminPassword: "long-admin-password",
            passphrase: Self.passphrase
        ))
        material = try Self.requireObject(ownerRequest["material"])
        vaultID = try Self.requireString(material["profile"], nestedKey: "vault_id")
        ownerTransferPublicKey = Self.base64URL(Data(repeating: 7, count: 32))
        qrPayload = try Self.json([
            "version": 1,
            "purpose": "connect_phone",
            "origin": Self.origin,
            "invitation_token": Self.base64URL(Data(repeating: 9, count: 32)),
            "transfer_public_key": ownerTransferPublicKey,
        ])
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        try lock.withLock {
            recordedRequests.append(request)
            switch (request.method, request.url.path) {
            case ("POST", "/v1/enrollment/invitations/claim"):
                let claim = try Self.object(String(data: try #require(request.body), encoding: .utf8)!)
                claimRequest = claim
                return try reply(request, malformedClaim ? ["origin": Self.origin] : claimResponse())
            case ("GET", "/v1/enrollment/invitations/\(invitationID)"):
                let status = try statusResponse()
                latestStatus = status
                return try reply(request, status)
            case ("POST", "/v1/enrollment/invitations/\(invitationID)/confirm"):
                if remainingConfirmationFailures > 0 {
                    remainingConfirmationFailures -= 1
                    return try reply(request, ["code": "temporarily_unavailable"], status: 503)
                }
                return try reply(request, [:])
            default:
                return HTTPResponse(status: 404, url: request.url, body: Data("{\"code\":\"not_found\"}".utf8))
            }
        }
    }

    private func claimResponse() throws -> [String: Any] {
        [
            "invitation_id": invitationID,
            "claim_secret": Self.base64URL(Data(repeating: 11, count: 32)),
            "origin": Self.origin,
            "vault_id": vaultID,
            "fingerprint": try Self.requireString(material["fingerprint"]),
            "key_epoch": try Self.requireInt(material["key_epoch"]),
            "method": method,
            "owner_transfer_public_key": ownerTransferPublicKey,
            "expires_in_seconds": 300,
        ]
    }

    private func statusResponse() throws -> [String: Any] {
        guard let claimRequest else { throw FixtureError.invalid }
        let device = try Self.requireObject(claimRequest["device"])
        let transcript: [String: Any] = [
            "origin": Self.origin,
            "vault_id": vaultID,
            "fingerprint": try Self.requireString(material["fingerprint"]),
            "key_epoch": try Self.requireInt(material["key_epoch"]),
            "enrollment_id": invitationID,
            "nonce": try Self.requireString(claimRequest["invitation_token_digest"]),
            "owner_transfer_public_key": ownerTransferPublicKey,
            "recipient_transfer_public_key": try Self.requireString(claimRequest["transfer_public_key"]),
            "signing_public_key": try Self.requireString(device["signing_public_key"]),
            "device_id": try Self.requireString(device["device_id"]),
            "credential_digest": try Self.requireString(claimRequest["credential_digest"]),
            "role": "gateway",
        ]
        return [
            "state": "approved",
            "expires_in_seconds": 300,
            "claim": NSNull(),
            "transcript": transcript,
            "encrypted_key_package": NSNull(),
            "transfer_nonce": NSNull(),
            "profile": material["profile"] ?? NSNull(),
        ]
    }


    var statusContainsMethodOrInvitationID: Bool {
        lock.withLock {
            guard let latestStatus else { return false }
            return latestStatus["method"] != nil || latestStatus["invitation_id"] != nil
        }
    }

    var confirmationAttempts: Int {
        paths.filter { $0 == "POST /v1/enrollment/invitations/\(invitationID)/confirm" }.count
    }

    var confirmationRequestHasProofs: Bool {
        lock.withLock {
            guard let request = recordedRequests.last(where: { $0.url.path.hasSuffix("/confirm") }),
                  let body = request.body,
                  let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else { return false }
            return Set(object.keys) == ["claim_secret", "transcript_hash", "device_signature", "admission_signature"]
        }
    }

    private func reply(_ request: HTTPRequest, _ object: [String: Any], status: Int = 200) throws -> HTTPResponse {
        HTTPResponse(status: status, url: request.url, body: try JSONSerialization.data(withJSONObject: object))
    }

    private static func object(_ json: String) throws -> [String: Any] {
        try requireObject(JSONSerialization.jsonObject(with: Data(json.utf8)))
    }

    private static func requireObject(_ value: Any?) throws -> [String: Any] {
        guard let object = value as? [String: Any] else { throw FixtureError.invalid }
        return object
    }

    private static func requireString(_ value: Any?) throws -> String {
        guard let value = value as? String else { throw FixtureError.invalid }
        return value
    }

    private static func requireString(_ value: Any?, nestedKey: String) throws -> String {
        try requireString(requireObject(value)[nestedKey])
    }

    private static func requireInt(_ value: Any?) throws -> Int {
        guard let value = value as? Int else { throw FixtureError.invalid }
        return value
    }

    private static func json(_ object: [String: Any]) throws -> String {
        String(data: try JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private enum FixtureError: Error {
        case invalid
    }
}
