import Foundation

/// The vault/device a credential and its local secrets are bound to.
public struct EnrollmentIdentity: Hashable, Sendable, Codable {
    public let origin: String
    public let vaultId: String
    public let deviceId: String

    /// Secure-store account name for one secret purpose of this enrollment.
    func account(_ purpose: String) -> String { "\(purpose)|\(origin)|\(vaultId)|\(deviceId)" }
}

/// The shared profile's rollback floor, carried by a v2 credential: the epoch floor and the
/// profile fingerprint at that epoch. Imported before any unlock of the enrollment.
struct EpochAnchor: Equatable, Sendable {
    let epochFloor: UInt32
    /// Lowercase hex fingerprint of the profile at `epochFloor`.
    let profileFingerprint: String
}

/// A strict device credential file, as written by operator/simulator pairing. Version 1 is
/// `{"version":1,"origin":…,"vaultId":…,"deviceId":…,"deviceToken":<96 hex>}`; version 2 adds
/// `epochFloor` (a positive u32) and `profileFingerprint` (64 lowercase hex).
public struct DeviceCredential: Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    public static let maxFileBytes = 4096
    public let origin: ServerOrigin
    public let identity: EnrollmentIdentity
    let token: String
    let anchor: EpochAnchor?

    private static let v1Fields: Set<String> = ["version", "origin", "vaultId", "deviceId", "deviceToken"]
    private static let v2Fields = v1Fields.union(["epochFloor", "profileFingerprint"])

    public static func parse(_ data: Data, allowLoopbackHTTP: Bool) throws(ClientError) -> DeviceCredential {
        guard data.count <= maxFileBytes else { throw .invalidCredential("file too large") }
        guard let object = try? JSONSerialization.jsonObject(with: data),
              let fields = object as? [String: Any]
        else { throw .invalidCredential("not a JSON object") }
        guard let version = fields["version"] as? NSNumber, !version.isBoolean, version == 1 || version == 2 else {
            throw .invalidCredential("unsupported version")
        }
        if version == 2, !hasUniqueMemberNames(data) {
            throw .invalidCredential("duplicate fields")
        }
        guard Set(fields.keys) == (version == 2 ? v2Fields : v1Fields) else {
            throw .invalidCredential("unexpected fields")
        }
        let anchor = try version == 2 ? parseAnchor(fields) : nil
        guard let rawOrigin = fields["origin"] as? String else { throw .invalidCredential("origin") }
        let origin = try ServerOrigin(canonical: rawOrigin, allowLoopbackHTTP: allowLoopbackHTTP)
        guard let vault = (fields["vaultId"] as? String).flatMap(UUID.init(uuidString:)),
              let device = (fields["deviceId"] as? String).flatMap(UUID.init(uuidString:))
        else { throw .invalidCredential("vault or device id") }
        guard let token = fields["deviceToken"] as? String, token.utf8.count == 96,
              token.utf8.allSatisfy(\.isASCIIHexDigit)
        else { throw .invalidCredential("device token") }
        return DeviceCredential(
            origin: origin,
            identity: EnrollmentIdentity(
                origin: origin.serialized,
                vaultId: vault.uuidString.lowercased(),
                deviceId: device.uuidString.lowercased()
            ),
            token: token,
            anchor: anchor
        )
    }

    /// JSONSerialization discards duplicate names, so inspect the original member strings first.
    private static func hasUniqueMemberNames(_ data: Data) -> Bool {
        let bytes = Array(data)
        var names = Set<String>()
        var index = 0
        while index < bytes.count {
            guard bytes[index] == 34 else { index += 1; continue }
            let start = index
            index += 1
            while index < bytes.count {
                if bytes[index] == 92 { index += 2; continue }
                if bytes[index] == 34 { break }
                index += 1
            }
            guard index < bytes.count else { return false }
            let end = index + 1
            index = end
            while index < bytes.count && [9, 10, 13, 32].contains(bytes[index]) { index += 1 }
            if index < bytes.count && bytes[index] == 58 {
                guard let name = try? JSONSerialization.jsonObject(
                    with: Data(bytes[start..<end]), options: .fragmentsAllowed
                ) as? String, names.insert(name).inserted else { return false }
            }
        }
        return true
    }

    /// The floor is a positive JSON integer that fits in u32 (no fraction, boolean or string);
    /// the fingerprint is 64 lowercase hex characters.
    static func parseAnchor(_ fields: [String: Any]) throws(ClientError) -> EpochAnchor {
        guard let floor = fields["epochFloor"] as? NSNumber, !floor.isBoolean, !CFNumberIsFloatType(floor as CFNumber),
              let epochFloor = UInt32(exactly: floor.int64Value), epochFloor > 0,
              let fingerprint = fields["profileFingerprint"] as? String, fingerprint.utf8.count == 64,
              fingerprint.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })
        else { throw .invalidCredential("epoch anchor") }
        return EpochAnchor(epochFloor: epochFloor, profileFingerprint: fingerprint)
    }

    public var description: String { "DeviceCredential(\(identity.origin), vault \(identity.vaultId), device \(identity.deviceId), token <redacted>)" }
    public var debugDescription: String { description }
    public var customMirror: Mirror { Mirror(self, children: ["identity": identity, "token": "<redacted>"]) }
}

extension NSNumber {
    var isBoolean: Bool { CFGetTypeID(self) == CFBooleanGetTypeID() }
}

extension UInt8 {
    var isASCIIHexDigit: Bool { (48...57).contains(self) || (65...70).contains(self) || (97...102).contains(self) }
}
