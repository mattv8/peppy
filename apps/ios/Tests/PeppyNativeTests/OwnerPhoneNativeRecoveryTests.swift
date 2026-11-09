import Foundation
import Testing
@testable import PeppyNative

@Suite("Native owner phone recovery")
struct OwnerPhoneNativeRecoveryTests {
    @Test func withdrawalRebuildsTheNativeOwnerAndApprovesANewInvitation() async throws {
        let fixture = try EnrollmentInstallFixture()
        let transport = OwnerRecoveryTransport(fixture: fixture)
        let store = InMemorySecureStore()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.claimInstance(origin: EnrollmentInstallFixture.origin, setupToken: "setup", username: "admin", adminPassword: EnrollmentInstallFixture.adminPassword, passphrase: EnrollmentInstallFixture.phrase, deviceName: "owner")
        let host = try await session.ownerPhoneApprovalHost(deviceName: "owner recovery")

        guard case let .view(_, qr: first?) = await host.start(method: .sharedSecret) else { Issue.record("expected first QR"); return }
        #expect(await host.cancel() == .cancelled)
        guard case let .view(_, qr: second?) = await host.start(method: .sharedSecret) else { Issue.record("expected replacement QR"); return }
        #expect(first != second)

        let recipient = try NativeEnrollmentRequestProducer(origin: EnrollmentInstallFixture.origin, deviceName: "recipient")
        let claim = try recipient.claimPhoneInvitationRequest(qr: second)
        try recipient.consumePhoneClaim(try transport.claimResponse(for: claim))
        let approved = await host.approve { nil }
        guard case let .view(status, _) = approved else { Issue.record("expected approved native invitation"); return }
        #expect(status.stage == .approved)
    }
}

private final class OwnerRecoveryTransport: HTTPTransport, @unchecked Sendable {
    private let fixture: EnrollmentInstallFixture
    private let lock = NSLock()
    private var invitation: [String: Any]?
    private var claim: [String: Any]?
    private var invitationID = "00000000-0000-0000-0000-000000000011"
    private var approved: [String: Any]?

    init(fixture: EnrollmentInstallFixture) { self.fixture = fixture }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let body = try lock.withLock { try response(for: request) }
        return HTTPResponse(status: 200, url: request.url, body: Data(body.utf8))
    }

    func claimResponse(for request: String) throws -> String { try lock.withLock { try claimResponseLocked(for: request) } }

    private func response(for request: HTTPRequest) throws -> String {
        let body = request.body.map { String(decoding: $0, as: UTF8.self) } ?? "{}"
        switch (request.method, request.url.path) {
        case ("POST", "/v1/instance/claim"):
            return try fixture.claimResponse(for: body)
        case ("POST", "/v1/enrollment/invitations"):
            invitation = try EnrollmentInstallFixture.object(body)
            invitationID = invitationID == "00000000-0000-0000-0000-000000000011" ? invitationID : "00000000-0000-0000-0000-000000000012"
            return EnrollmentInstallFixture.json(["invitation_id": invitationID, "expires_in_seconds": 300])
        case ("POST", "/v1/enrollment/invitations/claim"):
            return try claimResponseLocked(for: body)
        case ("GET", let path) where path.hasPrefix("/v1/enrollment/invitations/"):
            return try status()
        case ("DELETE", let path) where path.hasPrefix("/v1/enrollment/invitations/"):
            invitation = nil; claim = nil; approved = nil
            invitationID = "00000000-0000-0000-0000-000000000012"
            return "{}"
        case ("POST", let path) where path.hasSuffix("/approve"):
            let approval = try EnrollmentInstallFixture.object(body)
            var result = try EnrollmentInstallFixture.object(try status())
            result["state"] = "approved"
            result["encrypted_key_package"] = approval["encrypted_key_package"]
            result["transfer_nonce"] = approval["transfer_nonce"]
            result["profile"] = try EnrollmentInstallFixture.requiredObject(fixture.material, "profile")
            approved = result
            return EnrollmentInstallFixture.json(result)
        default: throw ClientError.invalidResponse("unexpected owner recovery request")
        }
    }

    private func claimResponseLocked(for request: String) throws -> String {
        let request = try EnrollmentInstallFixture.object(request)
        claim = request
        let invitation = try requiredInvitation()
        let profile = try EnrollmentInstallFixture.requiredObject(fixture.material, "profile")
        return EnrollmentInstallFixture.json([
            "invitation_id": invitationID, "claim_secret": base64URL(Data(repeating: 4, count: 32)),
            "origin": EnrollmentInstallFixture.origin, "vault_id": try EnrollmentInstallFixture.requiredString(profile, "vault_id"),
            "fingerprint": try EnrollmentInstallFixture.requiredString(fixture.material, "fingerprint"), "key_epoch": 1,
            "method": "shared_secret", "owner_transfer_public_key": try EnrollmentInstallFixture.requiredString(invitation, "transfer_public_key"), "expires_in_seconds": 300,
        ])
    }

    private func status() throws -> String {
        if let approved { return EnrollmentInstallFixture.json(approved) }
        guard claim != nil else {
            return EnrollmentInstallFixture.json([
                "state": "waiting", "expires_in_seconds": 300,
                "claim": NSNull(), "transcript": NSNull(), "encrypted_key_package": NSNull(), "transfer_nonce": NSNull(), "profile": NSNull(),
            ])
        }
        let invitation = try requiredInvitation()
        let claim = try requiredClaim()
        let device = try EnrollmentInstallFixture.requiredObject(claim, "device")
        let profile = try EnrollmentInstallFixture.requiredObject(fixture.material, "profile")
        return EnrollmentInstallFixture.json([
            "state": "claimed", "expires_in_seconds": 300,
            "claim": ["device": device, "transfer_public_key": try EnrollmentInstallFixture.requiredString(claim, "transfer_public_key"), "credential_digest": try EnrollmentInstallFixture.requiredString(claim, "credential_digest"), "claim_authenticator": try EnrollmentInstallFixture.requiredString(claim, "claim_authenticator")],
            "transcript": ["origin": EnrollmentInstallFixture.origin, "vault_id": try EnrollmentInstallFixture.requiredString(profile, "vault_id"), "fingerprint": try EnrollmentInstallFixture.requiredString(fixture.material, "fingerprint"), "key_epoch": 1, "enrollment_id": invitationID, "nonce": try EnrollmentInstallFixture.requiredString(claim, "invitation_token_digest"), "owner_transfer_public_key": try EnrollmentInstallFixture.requiredString(invitation, "transfer_public_key"), "recipient_transfer_public_key": try EnrollmentInstallFixture.requiredString(claim, "transfer_public_key"), "signing_public_key": try EnrollmentInstallFixture.requiredString(device, "signing_public_key"), "device_id": try EnrollmentInstallFixture.requiredString(device, "device_id"), "credential_digest": try EnrollmentInstallFixture.requiredString(claim, "credential_digest"), "role": "gateway"],
            "encrypted_key_package": NSNull(), "transfer_nonce": NSNull(), "profile": NSNull(),
        ])
    }

    private func requiredInvitation() throws -> [String: Any] { guard let invitation else { throw ClientError.invalidResponse("missing invitation") }; return invitation }
    private func requiredClaim() throws -> [String: Any] { guard let claim else { throw ClientError.invalidResponse("missing claim") }; return claim }
    private func base64URL(_ value: Data) -> String {
        value.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}
