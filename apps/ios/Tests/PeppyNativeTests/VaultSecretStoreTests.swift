import Foundation
import Testing
@testable import PeppyNative

@Suite struct VaultSecretStoreTests {
    private let identity = EnrollmentIdentity(origin: "https://vault.example.test", vaultId: "vault", deviceId: "device")
    private let origin = "https://vault.example.test"
    private var account: String { identity.account("vault-secret-v2") }
    private var pendingAccount: String { "vault-secret-pending-v2:\(origin)" }

    @Test func preservesExistingSecret() throws {
        let secure = VaultFaultyStore(); let store = VaultSecretStore(secure: secure)
        #expect(store.saveIfMissing("first", for: identity)); #expect(!store.saveIfMissing("second", for: identity)); #expect(try store.read(identity) == "first")
        #expect(secure.stored(account) == "first")
    }

    @Test func emptyValueIsMissing() throws {
        let secure = VaultFaultyStore(); try secure.upsert(account, Data())
        #expect(try VaultSecretStore(secure: secure).read(identity) == nil)
    }

    @Test func saveReplacesAnEmptyExistingValue() throws {
        let secure = VaultFaultyStore(); try secure.upsert(account, Data())
        #expect(VaultSecretStore(secure: secure).saveIfMissing("fresh", for: identity))
        #expect(secure.stored(account) == "fresh")
    }

    @Test func readFailureSavesNothingAndReportsFalse() {
        let secure = VaultFaultyStore(); secure.failReads = true
        #expect(!VaultSecretStore(secure: secure).saveIfMissing("fresh", for: identity))
        #expect(secure.stored(account) == nil)
    }

    @Test func readFailureSurfacesAsThrownError() {
        let secure = VaultFaultyStore(); secure.failReads = true
        #expect(throws: ClientError.self) { try VaultSecretStore(secure: secure).read(identity) }
    }

    @Test func writeFailureSavesNothingAndReportsFalse() {
        let secure = VaultFaultyStore(); secure.failWrites = true
        #expect(!VaultSecretStore(secure: secure).saveIfMissing("fresh", for: identity))
        #expect(secure.stored(account) == nil)
    }

    @Test func emptySecretIsNeverSaved() {
        let secure = VaultFaultyStore()
        #expect(!VaultSecretStore(secure: secure).saveIfMissing("", for: identity))
        #expect(secure.stored(account) == nil)
    }

    // MARK: Pending secret (creation-time save, stored to the binding after verified activation)

    @Test func pendingSecretRoundTrips() throws {
        let secure = VaultFaultyStore(); let store = VaultSecretStore(secure: secure)
        try store.savePending("pending secret", origin: origin)
        #expect(try store.pendingSecret(origin: origin) == "pending secret")
    }

    @Test func storedPendingSecretWinsOverAMistypedEntry() throws {
        let secure = VaultFaultyStore(); secure.seed(pendingAccount, "original pending")
        let chosen = try VaultSecretStore(secure: secure).candidatePendingSecret(origin: origin, entered: "mistyped retry")
        #expect(chosen == "original pending")
        #expect(secure.stored(pendingAccount) == "original pending")
    }

    @Test func enteredSecretIsUsedWhenNothingIsPending() throws {
        let secure = VaultFaultyStore()
        #expect(try VaultSecretStore(secure: secure).candidatePendingSecret(origin: origin, entered: "fresh") == "fresh")
        #expect(secure.stored(pendingAccount) == nil, "choosing a candidate must not write it")
    }

    @Test func missingPendingWithoutAnEntryIsRejected() {
        #expect(throws: ClientError.self) {
            try VaultSecretStore(secure: VaultFaultyStore()).candidatePendingSecret(origin: origin, entered: "")
        }
    }

    @Test func manualEntryPrefersTheEnteredSecretAndFallsBackToPending() throws {
        let secure = VaultFaultyStore(); secure.seed(pendingAccount, "pending secret")
        let store = VaultSecretStore(secure: secure)
        #expect(try store.sharedSecretForJoin(origin: origin, entered: "typed") == "typed")
        #expect(try store.sharedSecretForJoin(origin: origin, entered: "") == "pending secret")
    }

    @Test func manualEntryWithoutAnyEntryOrPendingIsRejected() {
        #expect(throws: ClientError.self) {
            try VaultSecretStore(secure: VaultFaultyStore()).sharedSecretForJoin(origin: origin, entered: "")
        }
    }

    @Test func storeBindingSavesMissingSecretAndTombstonesMatchingPending() throws {
        let secure = VaultFaultyStore(); secure.seed(pendingAccount, "pending secret")
        let store = VaultSecretStore(secure: secure)
        store.storeBinding("pending secret", for: identity, origin: origin)
        #expect(secure.stored(account) == "pending secret")
        #expect(try store.pendingSecret(origin: origin) == nil, "a stored pending copy must not outlive its binding")
    }

    @Test func storeBindingNeverOverwritesAnExistingBinding() throws {
        let secure = VaultFaultyStore(); secure.seed(account, "bound secret"); secure.seed(pendingAccount, "pending secret")
        VaultSecretStore(secure: secure).storeBinding("pending secret", for: identity, origin: origin)
        #expect(secure.stored(account) == "bound secret")
    }

    @Test func storeBindingKeepsAPendingCopyThatDiffersFromTheBinding() throws {
        let secure = VaultFaultyStore(); secure.seed(pendingAccount, "other vault secret")
        let store = VaultSecretStore(secure: secure)
        store.storeBinding("manual secret", for: identity, origin: origin)
        #expect(secure.stored(account) == "manual secret")
        #expect(try store.pendingSecret(origin: origin) == "other vault secret")
    }

    @Test func pendingWriteFailureThrowsSoCreationCanAbort() {
        let secure = VaultFaultyStore(); secure.failWrites = true
        do {
            try VaultSecretStore(secure: secure).savePending("pending secret", origin: origin)
            Issue.record("a failed pending write must abort creation")
        } catch {
            #expect(secure.stored(pendingAccount) == nil)
        }
    }

    @Test func pendingNonEmptyValueIsNotOverwritten() throws {
        let secure = VaultFaultyStore(); secure.seed(pendingAccount, "original pending")
        try VaultSecretStore(secure: secure).savePending("replacement", origin: origin)
        #expect(secure.stored(pendingAccount) == "original pending")
    }

    @Test func pendingEmptyLeftoverIsReplaced() throws {
        let secure = VaultFaultyStore(); try secure.upsert(pendingAccount, Data())
        let store = VaultSecretStore(secure: secure)
        try store.savePending("pending secret", origin: origin)
        #expect(try store.pendingSecret(origin: origin) == "pending secret")
    }

    @Test func emptyPendingSecretIsRejected() {
        let secure = VaultFaultyStore()
        do {
            try VaultSecretStore(secure: secure).savePending("", origin: origin)
            Issue.record("an empty pending secret must be rejected")
        } catch {
            #expect(secure.stored(pendingAccount) == nil)
        }
    }
}

private final class VaultFaultyStore: SecureStore, @unchecked Sendable {
    private let inner = InMemorySecureStore()
    var failReads = false
    var failWrites = false

    func seed(_ account: String, _ value: String) { try? inner.upsert(account, Data(value.utf8)) }
    func stored(_ account: String) -> String? { inner.value(account).flatMap { String(data: $0, encoding: .utf8) } }

    func read(_ account: String) throws(ClientError) -> Data? {
        if failReads { throw .unexpected("keychain read") }
        return inner.value(account)
    }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        if failWrites { throw .unexpected("keychain write") }
        return try inner.addIfAbsent(account, data)
    }
    func upsert(_ account: String, _ data: Data) throws(ClientError) {
        if failWrites { throw .unexpected("keychain write") }
        try inner.upsert(account, data)
    }
    func delete(_ account: String) throws(ClientError) { try inner.delete(account) }
}
