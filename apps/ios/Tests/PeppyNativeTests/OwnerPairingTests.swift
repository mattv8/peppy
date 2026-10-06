import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

@Suite struct OwnerPairingTests {
    private let passphrase = "owner pairing test passphrase"

    @Test func lockedAndNonOwnerDoNotCreateIntent() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let locked = try await enrolled(fixture, unlock: false)
        await #expect(throws: ClientError.forbidden) { try await locked.createOwnerPairingIntent() }
        #expect(!fixture.paths.contains("POST /v1/pairing/intents"))

        let nonOwner = try await enrolled(fixture, role: "device")
        await #expect(throws: ClientError.forbidden) { try await nonOwner.createOwnerPairingIntent() }
        #expect(!fixture.paths.contains("POST /v1/pairing/intents"))
    }

    @Test func createsExactQrOnlyAfterPinnedOwnerVaultCheck() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let intent = try await session.createOwnerPairingIntent()
        let qr = try #require(try JSONSerialization.jsonObject(with: intent.qrPayload) as? [String: Any])
        #expect(Set(qr.keys) == Set(["https_origin", "intent_token"]))
        #expect(qr["https_origin"] as? String == FakeServer.origin)
        #expect(qr["intent_token"] as? String == fixture.intent)
        #expect(fixture.createRequest?["https_origin"] as? String == FakeServer.origin)
    }

    @Test func rejectsForeignIntentOriginTokenAndBadClaimStatus() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        fixture.createOrigin = "https://other.example.test"
        await #expect(throws: ClientError.invalidResponse("pairing intent")) { try await session.createOwnerPairingIntent() }

        fixture.createOrigin = FakeServer.origin
        fixture.intent = "not-a-canonical-token"
        await #expect(throws: ClientError.invalidResponse("pairing intent")) { try await session.createOwnerPairingIntent() }

        fixture.intent = String(repeating: "A", count: 43)
        let intent = try await session.createOwnerPairingIntent()
        fixture.claimed = true
        fixture.requestedRole = "owner"
        await #expect(throws: ClientError.invalidResponse("pairing intent claim")) { try await session.ownerPairingClaim(intent) }
    }

    @Test func localSasMismatchAndFalseConfirmationNeverApprove() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let intent = try await session.createOwnerPairingIntent()
        fixture.claimed = true
        fixture.serverSas = "000000"
        await #expect(throws: ClientError.invalidResponse("pairing SAS")) { try await session.ownerPairingClaim(intent) }

        fixture.serverSas = nil
        let claim = try #require(try await session.ownerPairingClaim(intent))
        await #expect(throws: ClientError.forbidden) { try await session.approveOwnerPairing(intent, claim: claim, codesMatch: false) }
        #expect(!fixture.paths.contains("POST /v1/pairing/intents/\(fixture.intent)/approve"))
    }

    @Test func changedServerEpochCannotApproveAndCorrectApprovalPinsFields() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let intent = try await session.createOwnerPairingIntent()
        fixture.claimed = true
        let claim = try #require(try await session.ownerPairingClaim(intent))
        fixture.vaultEpoch = 2
        await #expect(throws: ClientError.identityMismatch) { try await session.approveOwnerPairing(intent, claim: claim, codesMatch: true) }
        #expect(fixture.approveRequest == nil)

        fixture.vaultEpoch = 1
        try await session.approveOwnerPairing(intent, claim: claim, codesMatch: true)
        #expect(fixture.approveRequest?["key_digest"] as? String == claim.keyDigest)
        #expect(fixture.approveRequest?["profile_fingerprint"] as? String == fixture.fingerprint)
        #expect(fixture.approveRequest?["key_epoch"] as? Int == 1)
    }

    @Test func intentIncludesServerExpiryAsDate() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        fixture.expiresInSeconds = 600
        let intent = try await session.createOwnerPairingIntent()
        let now = Date()
        let expectedExpiry = now.addingTimeInterval(TimeInterval(600))
        #expect(intent.expiresAt.timeIntervalSince1970 >= expectedExpiry.timeIntervalSince1970 - 1)
        #expect(intent.expiresAt.timeIntervalSince1970 <= expectedExpiry.timeIntervalSince1970 + 1)
    }

    @Test func offeringForeignJoinRequestMakesNoRequests() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        fixture.clearRequests()
        let qr = "{\"https_origin\":\"https://other.example.test\",\"join_request_id\":\"\(UUID().uuidString.lowercased())\",\"join_key\":\"CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}"

        await #expect(throws: ClientError.originMismatch) {
            try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))
        }
        #expect(fixture.paths.isEmpty)
    }

    @Test func offeringJoinRequestUsesOwnerAuthDigestAndSealedToken() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let requestId = UUID().uuidString.lowercased()
        let qr = "{\"https_origin\":\"\(FakeServer.origin)\",\"join_request_id\":\"\(requestId)\",\"join_key\":\"CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}"

        _ = try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))

        #expect(fixture.offerRequest?["intent_digest"] as? String == intentDigestHex(intentToken: fixture.intent))
        let sealed = try #require(fixture.offerRequest?["sealed_intent_token"] as? String)
        #expect(!sealed.isEmpty)
        #expect(sealed.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" })
        #expect(fixture.offerAuthorization == "Bearer \(fixture.ownerToken)")
        #expect(fixture.paths.contains("POST /v1/pairing/join-requests/\(requestId)/offer"))
    }

    @Test func offerMapsExpiredAndAlreadyOfferedResponses() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let qr = "{\"https_origin\":\"\(FakeServer.origin)\",\"join_request_id\":\"\(UUID().uuidString.lowercased())\",\"join_key\":\"CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}"

        fixture.offerStatus = (410, "join_request_expired")
        await #expect(throws: ClientError.server(status: 410, code: "join_request_expired")) {
            try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))
        }
        fixture.offerStatus = (409, "join_request_already_offered")
        await #expect(throws: ClientError.server(status: 409, code: "join_request_already_offered")) {
            try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))
        }
    }

    @Test func computerClaimMustUseDeviceRoleBeforeApproval() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let qr = "{\"https_origin\":\"\(FakeServer.origin)\",\"join_request_id\":\"\(UUID().uuidString.lowercased())\",\"join_key\":\"CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}"
        let intent = try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))
        fixture.claimed = true
        fixture.requestedRole = "device"
        let claim = try #require(try await session.ownerPairingClaim(intent))
        #expect(claim.requestedRole == "device")
        try await session.approveOwnerPairing(intent, claim: claim, codesMatch: true)
        #expect(fixture.approveRequest != nil)
    }

    @Test func cancelPathNeverApprovesComputerIntent() async throws {
        let fixture = try OwnerPairingTransport(passphrase: passphrase)
        let session = try await enrolled(fixture)
        let qr = "{\"https_origin\":\"\(FakeServer.origin)\",\"join_request_id\":\"\(UUID().uuidString.lowercased())\",\"join_key\":\"CQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}"
        _ = try await session.ownerOfferJoinRequest(qrData: Data(qr.utf8))

        #expect(fixture.approveRequest == nil)
    }

    private func enrolled(_ fixture: OwnerPairingTransport, role: String = "owner", unlock: Bool = true) async throws -> NativeSession {
        fixture.role = role
        let deviceId = UUID().uuidString.lowercased()
        let session = NativeSession(secureStore: InMemorySecureStore(), transport: fixture, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.importCredential(fixture.credential(deviceId: deviceId))
        if unlock { _ = try await session.unlock(passphrase: passphrase) }
        return session
    }
}

private final class OwnerPairingTransport: HTTPTransport, @unchecked Sendable {
    let fixture: FakeServer
    var intent = String(repeating: "A", count: 43)
    var role = "owner"
    var vaultEpoch = 1
    var createOrigin: String?
    var claimed = false
    var requestedRole = "gateway"
    var serverSas: String?
    var expiresInSeconds = 300
    var offerStatus = (200, "")
    private var enrolledDeviceId = ""
    let claimantId = UUID().uuidString.lowercased()
    let keyDigest = String(repeating: "b", count: 64)
    let fingerprint: String
    private(set) var paths: [String] = []
    private(set) var createRequest: [String: Any]?
    private(set) var approveRequest: [String: Any]?
    private(set) var offerRequest: [String: Any]?
    private(set) var offerAuthorization: String?
    var ownerToken: String { fixture.token }

    init(passphrase: String) throws {
        fixture = try FakeServer(passphrase: passphrase)
        fingerprint = try vaultProfileFingerprint(profileJson: fixture.material.profileJson)
    }

    func credential(deviceId: String) -> Data {
        enrolledDeviceId = deviceId
        return fixture.credential(deviceId: deviceId)
    }

    func clearRequests() { paths.removeAll() }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        paths.append("\(request.method) \(request.url.path)")
        func reply(_ status: Int, _ value: [String: Any]) -> HTTPResponse { fixture.reply(request, status, value) }
        switch (request.method, request.url.path) {
        case ("GET", "/v1/vault"):
            return reply(200, ["vault_id": fixture.vaultId, "device_id": enrolledDeviceId, "role": role, "key_epoch": vaultEpoch,
                               "profile_fingerprint": fingerprint,
                               "public_key_profile": try JSONSerialization.jsonObject(with: Data(fixture.material.profileJson.utf8)),
                               "encrypted_vault_check_header": Data(fixture.material.headerJson.utf8).base64EncodedString()])
        case ("POST", "/v1/pairing/intents"):
            createRequest = try JSONSerialization.jsonObject(with: request.body!) as? [String: Any]
            return reply(200, ["https_origin": createOrigin ?? FakeServer.origin, "intent_token": intent, "expires_in_seconds": expiresInSeconds])
        case ("POST", let path) where path.hasPrefix("/v1/pairing/join-requests/") && path.hasSuffix("/offer"):
            offerRequest = try JSONSerialization.jsonObject(with: request.body!) as? [String: Any]
            offerAuthorization = request.headers["Authorization"]
            return reply(offerStatus.0, offerStatus.0 == 200 ? [:] : ["code": offerStatus.1])
        case ("GET", let path) where path == "/v1/pairing/intents/\(intent)":
            let sas = try pairingIntentSas(intentToken: intent, keyDigest: keyDigest, deviceId: claimantId)
            return reply(200, ["claimed": claimed, "approved": false, "device_id": claimed ? claimantId : NSNull(),
                               "key_digest": claimed ? keyDigest : NSNull(), "requested_role": claimed ? requestedRole : NSNull(),
                               "sas": claimed ? (serverSas ?? sas) : NSNull(), "expires_in_seconds": expiresInSeconds])
        case ("POST", let path) where path == "/v1/pairing/intents/\(intent)/approve":
            approveRequest = try JSONSerialization.jsonObject(with: request.body!) as? [String: Any]
            return reply(200, ["challenge_token": String(repeating: "c", count: 43)])
        default: return reply(404, ["code": "not_found"])
        }
    }
}
