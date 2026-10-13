import Foundation

/// Per-binding vault secrets use the shared non-synchronizing, device-only Keychain store.
public struct VaultSecretStore: Sendable {
    private let secure: any SecureStore
    public init(secure: any SecureStore) { self.secure = secure }
    public func read(_ identity: EnrollmentIdentity) throws(ClientError) -> String? {
        guard let data = try secure.read(identity.account("vault-secret-v2")), !data.isEmpty else { return nil }
        guard let secret = String(data: data, encoding: .utf8), !secret.isEmpty else { throw .missingSecret("vault shared secret") }
        return secret
    }
    @discardableResult public func saveIfMissing(_ secret: String, for identity: EnrollmentIdentity) -> Bool {
        guard !secret.isEmpty else { return false }
        let account = identity.account("vault-secret-v2")
        do {
            if let existing = try secure.read(account), !existing.isEmpty { return false }
            if try secure.addIfAbsent(account, Data(secret.utf8)) { return true }
            guard let existing = try secure.read(account), existing.isEmpty else { return false }
            try secure.upsert(account, Data(secret.utf8)); return true
        } catch { return false }
    }

    /// Writes the creation-time secret. A non-empty pending value is never overwritten.
    public func savePending(_ secret: String, origin: String) throws(ClientError) {
        guard !secret.isEmpty else { throw .missingSecret("vault shared secret") }
        let account = pendingAccount(origin)
        if try secure.addIfAbsent(account, Data(secret.utf8)) { return }
        // An empty leftover is replaced so the write cannot silently lose the secret.
        guard let existing = try secure.read(account), !existing.isEmpty else {
            try secure.upsert(account, Data(secret.utf8))
            return
        }
    }

    /// Owner claim and vault creation reuse a stored pending secret. The entered value is used only when none is stored.
    public func candidatePendingSecret(origin: String, entered: String) throws(ClientError) -> String {
        if let pending = try readPending(origin) { return pending }
        guard !entered.isEmpty else { throw .missingSecret("vault shared secret") }
        return entered
    }

    /// Manual shared-secret admission uses the typed secret, or the pending secret when the entry is empty.
    public func sharedSecretForJoin(origin: String, entered: String) throws(ClientError) -> String {
        if !entered.isEmpty { return entered }
        guard let pending = try readPending(origin) else { throw .missingSecret("vault shared secret") }
        return pending
    }

    /// Stores a secret that passed verified activation under the device binding. An existing non-empty binding is kept.
    /// The pending copy is removed only when it equals the binding that now holds the secret.
    public func storeBinding(_ secret: String, for identity: EnrollmentIdentity, origin: String) {
        _ = saveIfMissing(secret, for: identity)
        guard (try? read(identity)) == secret, (try? readPending(origin)) == secret else { return }
        try? secure.delete(pendingAccount(origin))
    }

    public func pendingSecret(origin: String) throws(ClientError) -> String? { try readPending(origin) }

    private func readPending(_ origin: String) throws(ClientError) -> String? {
        guard let data = try secure.read(pendingAccount(origin)), !data.isEmpty else { return nil }
        guard let secret = String(data: data, encoding: .utf8), !secret.isEmpty else { throw .missingSecret("vault shared secret") }
        return secret
    }

    private func pendingAccount(_ origin: String) -> String { "vault-secret-pending-v2:\(origin)" }
}
