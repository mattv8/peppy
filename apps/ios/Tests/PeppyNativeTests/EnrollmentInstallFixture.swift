import Foundation
@testable import PeppyNative
import PeppyBindings

/// Native-produced enrollment material for host installation tests.  This never
/// manufactures an identity: the generated producer verifies every finish.
final class EnrollmentInstallFixture {
    static let origin = "https://enroll.example.test"
    static let phrase = "alpha bravo charlie delta echo foxtrot"
    static let adminPassword = "password long enough"

    let producer: NativeEnrollmentRequestProducer
    let material: [String: Any]
    private var manualDevice: [String: Any]?

    init() throws {
        producer = try NativeEnrollmentRequestProducer(origin: Self.origin, deviceName: "fixture owner")
        let request = try Self.object(producer.createOwnerRequest(
            setupToken: "setup", username: "admin", adminPassword: Self.adminPassword, passphrase: Self.phrase
        ))
        material = try Self.requiredObject(request, "material")
    }

    func claimResponse(for request: String) throws -> String {
        let object = try Self.object(request)
        let requestMaterial = try Self.requiredObject(object, "material")
        let profile = try Self.requiredObject(requestMaterial, "profile")
        let device = try Self.requiredObject(object, "device")
        return Self.json([
            "vault_id": try Self.requiredString(profile, "vault_id"),
            "device_id": try Self.requiredString(device, "device_id"),
            "replayed": false,
            "session": ["session_token": "fixture-native-session", "csrf_token": NSNull(), "expires_in_seconds": 3600],
        ])
    }

    func manualChallenge(for request: String) throws -> String {
        let object = try Self.object(request)
        let device = try Self.requiredObject(object, "device")
        manualDevice = device
        let profile = try Self.requiredObject(material, "profile")
        return Self.json([
            "enrollment_id": "00000000-0000-0000-0000-000000000007",
            "origin": Self.origin,
            "vault_id": try Self.requiredString(profile, "vault_id"),
            "profile": profile,
            "fingerprint": try Self.requiredString(material, "fingerprint"),
            "key_epoch": try Self.requiredInt(profile, "key_epoch"),
            "device": device,
            "requested_role": "gateway",
            "nonce": Data(repeating: 7, count: 32).base64URLEncodedString(),
            "credential_digest": try Self.requiredString(object, "credential_digest"),
            "expires_in_seconds": 300,
        ])
    }

    func admissionComplete(for request: String) throws -> String {
        let object = try Self.object(request)
        guard let device = (object["device"] as? [String: Any]) ?? manualDevice else {
            throw ClientError.invalidResponse("fixture device")
        }
        let profile = try Self.requiredObject(material, "profile")
        return Self.json([
            "vault_id": try Self.requiredString(profile, "vault_id"),
            "device_id": try Self.requiredString(device, "device_id"),
            "role": "gateway",
            "material": material,
        ])
    }

    func phoneInvitation(method: NativeEnrollmentMethod) throws -> PhoneInvitationFixture {
        let profile = try Self.requiredObject(material, "profile")
        let profileJSON = Self.json(profile)
        let header = try Self.requiredString(material, "check_header")
        let owner = try newNativeExistingOwnerEnrollmentClient(
            origin: Self.origin,
            kind: .ios,
            deviceName: "fixture phone owner",
            profileJson: profileJSON,
            headerJson: try Self.decodedHeader(header)
        )
        let invitation = try Self.object(owner.createPhoneInvitationRequest(method: method))
        let invitationID = "00000000-0000-0000-0000-000000000009"
        try owner.consumeCreatedPhoneInvitation(createdJson: EnrollmentInstallFixture.json([
            "invitation_id": invitationID,
            "expires_in_seconds": 300,
        ]))
        return try PhoneInvitationFixture(
            owner: owner,
            material: material,
            invitation: invitation,
            invitationID: invitationID,
            method: method,
            qrPayload: owner.ownerPhoneQrPayload()
        )
    }

    static func object(_ json: String) throws -> [String: Any] {
        guard let value = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any] else {
            throw ClientError.invalidResponse("fixture JSON")
        }
        return value
    }

    static func json(_ object: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    }

    static func requiredObject(_ object: [String: Any], _ key: String) throws -> [String: Any] {
        guard let value = object[key] as? [String: Any] else { throw ClientError.invalidResponse("fixture \(key)") }
        return value
    }

    static func requiredString(_ object: [String: Any], _ key: String) throws -> String {
        guard let value = object[key] as? String else { throw ClientError.invalidResponse("fixture \(key)") }
        return value
    }

    static func requiredInt(_ object: [String: Any], _ key: String) throws -> Int {
        guard let value = object[key] as? Int else { throw ClientError.invalidResponse("fixture \(key)") }
        return value
    }

    private static func decodedHeader(_ value: String) throws -> String {
        let padded = value.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
            .padding(toLength: ((value.count + 3) / 4) * 4, withPad: "=", startingAt: 0)
        guard let bytes = Data(base64Encoded: padded), let header = String(data: bytes, encoding: .utf8) else {
            throw ClientError.invalidResponse("fixture check_header")
        }
        return header
    }
}

final class PhoneInvitationFixture {
    private let owner: NativeEnrollmentClient
    private let material: [String: Any]
    private let invitation: [String: Any]
    private let invitationID: String
    private let method: NativeEnrollmentMethod
    let qrPayload: String
    private var claim: [String: Any]?
    private var approved: [String: Any]?

    init(owner: NativeEnrollmentClient, material: [String: Any], invitation: [String: Any], invitationID: String, method: NativeEnrollmentMethod, qrPayload: String) throws {
        self.owner = owner
        self.material = material
        self.invitation = invitation
        self.invitationID = invitationID
        self.method = method
        self.qrPayload = qrPayload
    }

    func consumeClaim(_ request: String) throws -> String {
        let request = try EnrollmentInstallFixture.object(request)
        claim = request
        let profile = try EnrollmentInstallFixture.requiredObject(material, "profile")
        return EnrollmentInstallFixture.json([
            "invitation_id": invitationID,
            "claim_secret": Data(repeating: 4, count: 32).base64URLEncodedString(),
            "origin": EnrollmentInstallFixture.origin,
            "vault_id": try EnrollmentInstallFixture.requiredString(profile, "vault_id"),
            "fingerprint": try EnrollmentInstallFixture.requiredString(material, "fingerprint"),
            "key_epoch": 1,
            "method": method == .transfer ? "transfer" : "shared_secret",
            "owner_transfer_public_key": try EnrollmentInstallFixture.requiredString(invitation, "transfer_public_key"),
            "expires_in_seconds": 300,
        ])
    }

    func approve() throws {
        let claimed = try status(state: "claimed")
        let approval = try EnrollmentInstallFixture.object(owner.approvePhoneInvitationRequest(
            statusJson: claimed,
            passphrase: EnrollmentInstallFixture.phrase
        ))
        var approved = try EnrollmentInstallFixture.object(claimed)
        approved["state"] = "approved"
        approved["encrypted_key_package"] = approval["encrypted_key_package"]
        approved["transfer_nonce"] = approval["transfer_nonce"]
        approved["profile"] = try EnrollmentInstallFixture.requiredObject(material, "profile")
        self.approved = approved
    }

    func status() throws -> String {
        if let approved { return EnrollmentInstallFixture.json(approved) }
        return try status(state: "claimed")
    }

    func admissionComplete() throws -> String {
        let device = try EnrollmentInstallFixture.requiredObject(try requiredClaim(), "device")
        return EnrollmentInstallFixture.json([
            "vault_id": try EnrollmentInstallFixture.requiredString(try EnrollmentInstallFixture.requiredObject(material, "profile"), "vault_id"),
            "device_id": try EnrollmentInstallFixture.requiredString(device, "device_id"),
            "role": "gateway",
            "material": material,
        ])
    }

    private func status(state: String) throws -> String {
        let claim = try requiredClaim()
        let device = try EnrollmentInstallFixture.requiredObject(claim, "device")
        let transcript: [String: Any] = [
            "origin": EnrollmentInstallFixture.origin,
            "vault_id": try EnrollmentInstallFixture.requiredString(try EnrollmentInstallFixture.requiredObject(material, "profile"), "vault_id"),
            "fingerprint": try EnrollmentInstallFixture.requiredString(material, "fingerprint"),
            "key_epoch": 1,
            "enrollment_id": invitationID,
            "nonce": try EnrollmentInstallFixture.requiredString(claim, "invitation_token_digest"),
            "owner_transfer_public_key": try EnrollmentInstallFixture.requiredString(invitation, "transfer_public_key"),
            "recipient_transfer_public_key": try EnrollmentInstallFixture.requiredString(claim, "transfer_public_key"),
            "signing_public_key": try EnrollmentInstallFixture.requiredString(device, "signing_public_key"),
            "device_id": try EnrollmentInstallFixture.requiredString(device, "device_id"),
            "credential_digest": try EnrollmentInstallFixture.requiredString(claim, "credential_digest"),
            "role": "gateway",
        ]
        return EnrollmentInstallFixture.json([
            "state": state,
            "expires_in_seconds": 300,
            "claim": [
                "device": device,
                "transfer_public_key": try EnrollmentInstallFixture.requiredString(claim, "transfer_public_key"),
                "credential_digest": try EnrollmentInstallFixture.requiredString(claim, "credential_digest"),
                "claim_authenticator": try EnrollmentInstallFixture.requiredString(claim, "claim_authenticator"),
            ],
            "transcript": transcript,
            "encrypted_key_package": NSNull(),
            "transfer_nonce": NSNull(),
            "profile": NSNull(),
        ])
    }

    private func requiredClaim() throws -> [String: Any] {
        guard let claim else { throw ClientError.invalidResponse("phone claim before status") }
        return claim
    }
}

private extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}
