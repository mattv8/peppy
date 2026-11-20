import Foundation
import Testing
@testable import PeppyNative

@Suite("Native enrollment installation")
struct EnrollmentInstallIntegrationTests {
    @Test(arguments: [false, true], [1, 2])
    func pendingImportPinFencesVerifiedPhoneInstallation(sharedSecret: Bool, floor: Int) async throws {
        let fixture = try EnrollmentInstallFixture()
        let phone = try fixture.phoneInvitation(method: sharedSecret ? .sharedSecret : .transfer)
        let secure = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: secure, transport: InstallTransport(fixture: fixture, phone: phone), databaseDirectory: directory, allowLoopbackHTTP: false)
        _ = try await session.beginPhoneEnrollment(qrPayload: phone.qrPayload, deviceName: "pending anchor")
        try phone.approve()
        let admission = try EnrollmentInstallFixture.object(phone.admissionComplete())
        let identity = EnrollmentIdentity(
            origin: EnrollmentInstallFixture.origin,
            vaultId: try EnrollmentInstallFixture.requiredString(admission, "vault_id"),
            deviceId: try EnrollmentInstallFixture.requiredString(admission, "device_id")
        )
        var header = try EnrollmentInstallFixture.requiredString(fixture.material, "check_header")
            .replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        header += String(repeating: "=", count: (4 - header.count % 4) % 4)
        let vault: [String: Any] = [
            "vault_id": identity.vaultId, "device_id": identity.deviceId, "role": "gateway",
            "key_epoch": floor, "public_key_profile": fixture.material["profile"]!,
            "encrypted_vault_check_header": header,
        ]
        let credential: [String: Any] = [
            "version": 2, "origin": identity.origin, "vaultId": identity.vaultId, "deviceId": identity.deviceId,
            "deviceToken": String(repeating: "ab", count: 48), "epochFloor": floor,
            "profileFingerprint": String(repeating: "aa", count: 32),
        ]
        // Fail database creation AFTER activating the imported secure record.
        let blockedDirectory = temporaryDirectory().appendingPathComponent("file-not-directory")
        try Data([0]).write(to: blockedDirectory)
        let interrupted = NativeSession(
            secureStore: secure, transport: PendingImportVaultTransport(body: try JSONSerialization.data(withJSONObject: vault)),
            databaseDirectory: blockedDirectory, allowLoopbackHTTP: false
        )
        await #expect(throws: Error.self) { try await interrupted.importCredential(JSONSerialization.data(withJSONObject: credential)) }
        let pending = try EnrollmentStore(secure: secure).record(identity)
        #expect(pending.epochFloor == UInt32(floor))
        #expect(secure.value(identity.account("database-created")) == nil)

        // The handoff binding intentionally maps activation errors to Crypto; phrase
        // unlock preserves the more specific InvalidProfile mapping.
        let expectedError = ClientError.core(sharedSecret ? .InvalidProfile : .Crypto)
        await #expect(throws: expectedError) {
            try await session.confirmPhoneEnrollment(sharedSecret: sharedSecret ? EnrollmentInstallFixture.phrase : nil)
        }
        #expect(try EnrollmentStore(secure: secure).record(identity) == pending)
        #expect(secure.value(identity.account("key-cache-1")) == nil)
        await #expect(throws: ClientError.notEnrolled) { try await session.conversations() }
        await session.close()
    }

    @Test func ownerClaimInstallsVerifiedPhraseAndReopensWithTheNativeCache() async throws {
        let fixture = try EnrollmentInstallFixture()
        let transport = InstallTransport(fixture: fixture)
        let store = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        let installed = try await session.claimInstance(origin: EnrollmentInstallFixture.origin, setupToken: "setup", username: "admin", adminPassword: EnrollmentInstallFixture.adminPassword, passphrase: EnrollmentInstallFixture.phrase, deviceName: "owner")
        #expect(installed.keysUnlocked)
        let identity = try #require(installed.identity)
        #expect(store.value(identity.account("device-token")) != nil)

        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)
    }

    @Test func manualAdmissionUsesTheGeneratedMaterialAndRejectsAWrongPhraseBeforeStorage() async throws {
        let fixture = try EnrollmentInstallFixture()
        let transport = InstallTransport(fixture: fixture)
        let store = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        let installed = try await session.joinWithSharedSecret(origin: EnrollmentInstallFixture.origin, vaultID: nil, vaultOwner: nil, passphrase: EnrollmentInstallFixture.phrase, deviceName: "manual")
        #expect(installed.keysUnlocked)
        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)

        let rejectedStore = InMemorySecureStore()
        let rejected = NativeSession(secureStore: rejectedStore, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        await #expect(throws: Error.self) {
            _ = try await rejected.joinWithSharedSecret(origin: EnrollmentInstallFixture.origin, vaultID: nil, vaultOwner: nil, passphrase: "wrong phrase", deviceName: "manual")
        }
        #expect(try EnrollmentStore(secure: rejectedStore).activeIdentity() == nil)
        #expect(!rejectedStore.accounts.contains { $0.contains("device-token") || $0.contains("key-cache-") })
    }

    @Test func approvedVaultRequestInstallsAndPreservesTheExistingBindingSecret() async throws {
        let fixture = try EnrollmentInstallFixture()
        let transport = InstallTransport(fixture: fixture)
        let store = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        let invite = Data(repeating: 3, count: 32).base64EncodedString().replacingOccurrences(of: "=", with: "")

        _ = try await session.beginVaultRequest(inviteLink: "\(EnrollmentInstallFixture.origin)/#invite=\(invite)", displayName: "fixture", expectedOrigin: nil, deviceName: "vault")
        _ = try await session.vaultRequestStatus(origin: EnrollmentInstallFixture.origin)
        let installed = try await session.createVault(origin: EnrollmentInstallFixture.origin, username: "admin", adminPassword: EnrollmentInstallFixture.adminPassword, passphrase: EnrollmentInstallFixture.phrase)
        #expect(installed.keysUnlocked)
        let identity = try #require(installed.identity)
        #expect(try VaultSecretStore(secure: store).read(identity) == EnrollmentInstallFixture.phrase)
        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)
    }

    @Test func sharedPhoneAdmissionInstallsTheVerifiedPhraseAndReopens() async throws {
        let fixture = try EnrollmentInstallFixture()
        let phone = try fixture.phoneInvitation(method: .sharedSecret)
        let transport = InstallTransport(fixture: fixture, phone: phone, losesFirstPhoneConfirmation: true)
        let store = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        let waiting = try await session.beginPhoneEnrollment(qrPayload: phone.qrPayload, deviceName: "shared phone")
        #expect(waiting.requiresSharedSecret)
        try phone.approve()
        await #expect(throws: Error.self) {
            _ = try await session.confirmPhoneEnrollment(sharedSecret: EnrollmentInstallFixture.phrase)
        }
        let installed = try await session.confirmPhoneEnrollment(sharedSecret: nil)
        #expect(installed.keysUnlocked)
        let identity = try #require(installed.identity)
        #expect(try VaultSecretStore(secure: store).read(identity) == EnrollmentInstallFixture.phrase)

        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)
    }

    @Test func transferPhoneAdmissionInstallsTheHandoffAndReopens() async throws {
        let fixture = try EnrollmentInstallFixture()
        let phone = try fixture.phoneInvitation(method: .transfer)
        let transport = InstallTransport(fixture: fixture, phone: phone)
        let store = InMemorySecureStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        let waiting = try await session.beginPhoneEnrollment(qrPayload: phone.qrPayload, deviceName: "transfer phone")
        #expect(!waiting.requiresSharedSecret)
        try phone.approve()
        let installed = try await session.confirmPhoneEnrollment(sharedSecret: nil)
        #expect(installed.keysUnlocked)
        #expect(try #require(installed.identity).account("device-token").isEmpty == false)

        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)
    }

    @Test func bindingSecretWriteFailureKeepsTheVerifiedCacheReopenable() async throws {
        let fixture = try EnrollmentInstallFixture()
        let transport = InstallTransport(fixture: fixture)
        let secure = BindingWriteFailingStore()
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: secure, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        let installed = try await session.claimInstance(origin: EnrollmentInstallFixture.origin, setupToken: "setup", username: "admin", adminPassword: EnrollmentInstallFixture.adminPassword, passphrase: EnrollmentInstallFixture.phrase, deviceName: "binding failure")
        let identity = try #require(installed.identity)
        #expect(installed.keysUnlocked)
        #expect(try VaultSecretStore(secure: secure).read(identity) == nil)
        #expect(secure.value(identity.account("device-token")) != nil)
        #expect(secure.accounts.contains { $0.contains("key-cache-") })

        await session.close()
        let reopened = NativeSession(secureStore: secure, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        #expect(try await reopened.open().keysUnlocked)
    }

    @Test(arguments: [false, true], ["active-enrollment", "enrollment", "key-cache-"])
    func phoneStorageFailuresPreserveWriteOrderAndReopenState(sharedSecret: Bool, failure: String) async throws {
        let fixture = try EnrollmentInstallFixture()
        let phone = try fixture.phoneInvitation(method: sharedSecret ? .sharedSecret : .transfer)
        let transport = InstallTransport(fixture: fixture, phone: phone)
        let store = InstallWriteFailingStore(failure: failure)
        let directory = temporaryDirectory()
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)

        _ = try await session.beginPhoneEnrollment(qrPayload: phone.qrPayload, deviceName: "phone")
        try phone.approve()
        await #expect(throws: ClientError.secureStorage(status: -1)) {
            _ = try await session.confirmPhoneEnrollment(sharedSecret: sharedSecret ? EnrollmentInstallFixture.phrase : nil)
        }
        let expected = ["device-token", "enrollment", "active-enrollment", "key-cache-"]
        let failureIndex = try #require(expected.firstIndex(of: failure))
        #expect(store.writes == Array(expected.prefix(failureIndex + 1)))
        #expect(!store.accounts.contains { $0.hasPrefix("vault-secret-v2") })

        await session.close()
        let reopened = NativeSession(secureStore: store, transport: transport, databaseDirectory: directory, allowLoopbackHTTP: false)
        let status = try await reopened.open()
        #expect(status.databaseOpen == (failure == "key-cache-"))
        #expect(!status.keysUnlocked)
        await reopened.close()
    }
}

private struct PendingImportVaultTransport: HTTPTransport {
    let body: Data
    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        guard request.method == "GET", request.url.path == "/v1/vault" else {
            throw ClientError.invalidResponse("unexpected pending-import request")
        }
        return HTTPResponse(status: 200, url: request.url, body: body)
    }
}

private final class InstallWriteFailingStore: SecureStore, @unchecked Sendable {
    private let inner = InMemorySecureStore()
    private let failure: String
    private(set) var writes: [String] = []
    var accounts: Set<String> { inner.accounts }

    init(failure: String) { self.failure = failure }
    func read(_ account: String) throws(ClientError) -> Data? { try inner.read(account) }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool { try inner.addIfAbsent(account, data) }
    func upsert(_ account: String, _ data: Data) throws(ClientError) {
        if let purpose = ["device-token", "enrollment", "active-enrollment", "key-cache-"].first(where: {
            account == $0 || account.hasPrefix("\($0)|") || ($0 == "key-cache-" && account.hasPrefix($0))
        }) {
            writes.append(purpose)
            if purpose == failure { throw .secureStorage(status: -1) }
        }
        try inner.upsert(account, data)
    }
    func delete(_ account: String) throws(ClientError) { try inner.delete(account) }
}

private final class BindingWriteFailingStore: SecureStore, @unchecked Sendable {
    private let inner = InMemorySecureStore()
    var accounts: Set<String> { inner.accounts }
    func value(_ account: String) -> Data? { inner.value(account) }
    func read(_ account: String) throws(ClientError) -> Data? { try inner.read(account) }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        if account.contains("vault-secret-v2") { throw .secureStorage(status: -1) }
        return try inner.addIfAbsent(account, data)
    }
    func upsert(_ account: String, _ data: Data) throws(ClientError) { try inner.upsert(account, data) }
    func delete(_ account: String) throws(ClientError) { try inner.delete(account) }
}

private final class InstallTransport: HTTPTransport, @unchecked Sendable {
    private let fixture: EnrollmentInstallFixture
    private let phone: PhoneInvitationFixture?
    private var losesFirstPhoneConfirmation: Bool
    private let lock = NSLock()
    private var vaultRequestID = "00000000-0000-0000-0000-000000000008"

    init(fixture: EnrollmentInstallFixture, phone: PhoneInvitationFixture? = nil, losesFirstPhoneConfirmation: Bool = false) {
        self.fixture = fixture
        self.phone = phone
        self.losesFirstPhoneConfirmation = losesFirstPhoneConfirmation
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let body = try lock.withLock { try responseBody(for: request) }
        return HTTPResponse(status: 200, url: request.url, body: Data(body.utf8))
    }

    private func responseBody(for request: HTTPRequest) throws -> String {
        let body = request.body.map { String(decoding: $0, as: UTF8.self) } ?? "{}"
        switch (request.method, request.url.path) {
        case ("POST", "/v1/instance/claim"), ("POST", "/v1/instance/vaults"):
            return try fixture.claimResponse(for: body)
        case ("POST", "/v1/enrollment/challenges"):
            return try fixture.manualChallenge(for: body)
        case ("POST", "/v1/enrollment/invitations/claim"):
            return try phoneResponse().consumeClaim(body)
        case ("GET", let path) where path.hasPrefix("/v1/enrollment/invitations/"):
            return try phoneResponse().status()
        case ("POST", let path) where path.hasSuffix("/confirm"):
            if losesFirstPhoneConfirmation {
                losesFirstPhoneConfirmation = false
                throw ClientError.network(code: -1)
            }
            return try phoneResponse().admissionComplete()
        case ("POST", let path) where path.hasSuffix("/complete"):
            return try fixture.admissionComplete(for: body)
        case ("POST", "/v1/instance/vault-requests"):
            let requestObject = try EnrollmentInstallFixture.object(body)
            vaultRequestID = try EnrollmentInstallFixture.requiredString(requestObject, "request_id")
            return EnrollmentInstallFixture.json(["request_id": vaultRequestID, "expires_in_seconds": 300])
        case ("GET", let path) where path.hasPrefix("/v1/instance/vault-requests/"):
            return EnrollmentInstallFixture.json(["request_id": vaultRequestID, "state": "approved", "expires_in_seconds": 300])
        default:
            throw ClientError.invalidResponse("unexpected fixture request \(request.method) \(request.url.path)")
        }
    }

    private func phoneResponse() throws -> PhoneInvitationFixture {
        guard let phone else { throw ClientError.invalidResponse("unexpected phone fixture request") }
        return phone
    }
}
