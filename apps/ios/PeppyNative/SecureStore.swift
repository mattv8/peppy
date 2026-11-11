import Foundation
import Security

/// OS secure storage boundary. Only this protocol is replaced in tests.
public protocol SecureStore: Sendable {
    func read(_ account: String) throws(ClientError) -> Data?
    /// Stores `data` only if no item exists. Returns `false` (and leaves the item untouched) otherwise.
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool
    func upsert(_ account: String, _ data: Data) throws(ClientError)
    /// Removes an item; a missing item is not an error.
    func delete(_ account: String) throws(ClientError)
}

/// Keychain generic-password items, readable after first unlock, never synchronized and never
/// migrated to another device (`…ThisDeviceOnly`). No backup/reinstall restoration is claimed.
public struct KeychainSecureStore: SecureStore {
    public let service: String

    public init(service: String = "dev.peppy.mobile") { self.service = service }

    func query(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: kCFBooleanFalse as Any,
            kSecUseDataProtectionKeychain as String: true,
        ]
    }

    static var accessibility: CFString { kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly }

    func addAttributes(account: String, data: Data) -> [String: Any] {
        var attributes = query(account)
        attributes[kSecValueData as String] = data
        attributes[kSecAttrAccessible as String] = Self.accessibility
        return attributes
    }

    func updateAttributes(data: Data) -> [String: Any] {
        [
            kSecValueData as String: data,
            kSecAttrAccessible as String: Self.accessibility,
            kSecAttrSynchronizable as String: kCFBooleanFalse as Any,
            kSecUseDataProtectionKeychain as String: true,
        ]
    }

    public func read(_ account: String) throws(ClientError) -> Data? {
        var request = query(account)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw .secureStorage(status: status) }
        return data
    }

    public func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        let status = SecItemAdd(addAttributes(account: account, data: data) as CFDictionary, nil)
        if status == errSecDuplicateItem { return false }
        guard status == errSecSuccess else { throw .secureStorage(status: status) }
        return true
    }

    public func upsert(_ account: String, _ data: Data) throws(ClientError) {
        if try addIfAbsent(account, data) { return }
        let status = SecItemUpdate(query(account) as CFDictionary, updateAttributes(data: data) as CFDictionary)
        guard status == errSecSuccess else { throw .secureStorage(status: status) }
    }

    public func delete(_ account: String) throws(ClientError) {
        let status = SecItemDelete(query(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw .secureStorage(status: status) }
    }
}

/// Reinstall-stable, device-local succession hint. It is deliberately not scoped to an enrollment:
/// a replacement pairing needs the same opaque value to succeed the previous device.
struct DeviceHintStore: Sendable {
    static let account = "device-hint-v1"
    static let byteCount = 32

    let secure: any SecureStore

    func readOrCreate() throws(ClientError) -> Data {
        if let hint = try secure.read(Self.account) {
            guard hint.count == Self.byteCount else { throw .invalidResponse("device hint") }
            return hint
        }

        var bytes = Data(count: Self.byteCount)
        let status = bytes.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, Self.byteCount, $0.baseAddress!) }
        guard status == errSecSuccess else { throw .secureStorage(status: status) }
        if try secure.addIfAbsent(Self.account, bytes) { return bytes }

        guard let stored = try secure.read(Self.account), stored.count == Self.byteCount else {
            throw .invalidResponse("device hint")
        }
        return stored
    }
}
