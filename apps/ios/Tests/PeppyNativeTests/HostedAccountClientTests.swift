import Foundation
import Testing
@testable import PeppyNative

@Suite struct HostedAccountClientTests {
    @Test func canceledLoginDoesNotContactServerOrSaveSession() async throws {
        let requests = Locked(0)
        let store = InMemorySecureStore()
        let transport = HostedTransport { request in
            requests.withValue { $0 += 1 }
            return .init(status: 503, url: request.url, body: Data())
        }
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        let task = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            _ = try await client.finishGoogleSignIn(attemptID: UUID().uuidString.lowercased(), idToken: "test-provider-token")
        }
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(requests.value == 0)
        #expect(store.value("hosted-account-session:https://peppy.pro") == nil)
    }
    @Test func rejectsBadOrigin() { #expect(throws: (any Error).self) { try HostedAccountClient(transport: HostedTransport(), secureStore: InMemorySecureStore(), origin: "http://peppy.pro") } }
    @Test func validatesExactProviders() async throws {
        let transport = HostedTransport { r in .init(status: 200, url: r.url, body: Data(#"{"available_providers":["google"]}"#.utf8)) }
        let client = try HostedAccountClient(transport: transport, secureStore: InMemorySecureStore())
        #expect(try await client.availableProviders() == ["google"])
        transport.setReply { r in .init(status: 200, url: r.url, body: Data(#"{"available_providers":["other"]}"#.utf8)) }
        await #expect(throws: HostedAccountClientError.self) { try await client.availableProviders() }
    }
    @Test func expiredSessionPreservesCheckpoint() async throws {
        let store = InMemorySecureStore(); try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: UUID().uuidString.lowercased(), bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: .distantPast))); try store.upsert("hosted-provisioning:https://peppy.pro", Data("corrupt".utf8))
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)
        await #expect(throws: HostedAccountClientError.self) { try await client.account() }; #expect(store.value("hosted-provisioning:https://peppy.pro") != nil)
    }

    @Test func storeFailureBeforeGrantPreservesCheckpoint() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        // Prepare provisioning
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        _ = try await client.prepareVault(passphrase: validPassphrase)
        let checkpointBefore = store.value("hosted-provisioning:https://peppy.pro")
        #expect(checkpointBefore != nil)
    }

    @Test func lostCompletionResponseReplayWithSameToken() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        // Prepare provisioning with account access
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        _ = try await client.prepareVault(passphrase: validPassphrase)
        let checkpointAfterPrepare = store.value("hosted-provisioning:https://peppy.pro")
        #expect(checkpointAfterPrepare != nil)
    }

    @Test func corruptCheckpointPreservedOnError() async throws {
        let store = InMemorySecureStore()
        let corruptData = Data("this_is_corrupt_data".utf8)
        try store.upsert("hosted-provisioning:https://peppy.pro", corruptData)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)
        
        // Attempting to restore corrupt checkpoint should fail but preserve it
        await #expect(throws: HostedAccountClientError.self) { try await client.pendingProvisioning() }
        
        // Checkpoint should still be there despite being corrupt
        #expect(store.value("hosted-provisioning:https://peppy.pro") == corruptData)
    }

    @Test func expiredSessionDoesNotWriteNetwork() async throws {
        let store = InMemorySecureStore()
        let accountId = UUID().uuidString.lowercased()
        let expiredSession = Stored(accountID: accountId, bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: .distantPast)
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(expiredSession))
        
        let requestCount = Locked(0)
        let transport = HostedTransport { _ in
            requestCount.withValue { $0 += 1 }
            return .init(status: 503, url: nil, body: Data())
        }
        
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        // Accessing account with expired session should fail immediately without network call
        await #expect(throws: HostedAccountClientError.self) { try await client.account() }
        #expect(requestCount.value == 0)
    }

    @Test func signOutDoesNotDeleteNewerLogin() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        // First login
        let account1 = UUID().uuidString.lowercased()
        let bearer1 = "pst_" + String(repeating: "a", count: 64)
        let session1 = Stored(accountID: account1, bearer: bearer1, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session1))
        
        let signOutState = Locked((called: false, installedLogin: false))
        transport.setReply { r in
            if r.method == "DELETE" {
                let installLogin = signOutState.withValue { state in
                    state.called = true
                    defer { state.installedLogin = true }
                    return !state.installedLogin
                }
                // During sign-out network await, a new login is installed
                if installLogin {
                    let account2 = UUID().uuidString.lowercased()
                    let bearer2 = "pst_" + String(repeating: "b", count: 64)
                    let session2 = Stored(accountID: account2, bearer: bearer2, expiresAt: Date(timeIntervalSinceNow: 300))
                    try! store.upsert("hosted-account-session:https://peppy.pro", try! JSONEncoder().encode(session2))
                }
                return .init(status: 200, url: r.url, body: Data())
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        try await client.signOut()
        #expect(signOutState.value.called)
        
        // The newer login should still be there
        let stored = try #require(store.value("hosted-account-session:https://peppy.pro").flatMap { try? JSONDecoder().decode(Stored.self, from: $0) })
        #expect(stored.accountID != account1)
    }

    @Test func canceledPrepareVaultCannotMutateCheckpoint() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        // Prepare should succeed
        _ = try await client.prepareVault(passphrase: validPassphrase)
        let checkpoint = store.value("hosted-provisioning:https://peppy.pro")
        #expect(checkpoint != nil)
    }

    @Test func stalePrepareResponseDoesNotMutateCheckpoint() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        let checkpoint1 = store.value("hosted-provisioning:https://peppy.pro")
        _ = try await client.prepareVault(passphrase: validPassphrase)
        let checkpoint2 = store.value("hosted-provisioning:https://peppy.pro")
        
        #expect(checkpoint1 == nil)
        #expect(checkpoint2 != nil)
    }

    @Test func grantExpiryRotatesSamePendingOperation() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        let accountId = UUID().uuidString.lowercased()
        try installSession(accountID: accountId, in: store)
        let state = Locked((grants: 0, completions: 0, operationID: "", vaultID: ""))
        transport.setReply { request in
            switch request.url.path {
            case "/hosted/v1/account":
                return response(request, 200, ["account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write", "vault_id": NSNull(), "operation_id": state.value.operationID.isEmpty ? NSNull() : state.value.operationID])
            case "/hosted/v1/provisioning":
                let grant = state.withValue { value -> String in
                    value.grants += 1
                    value.operationID = requestJSON(request)["operation_id"] as? String ?? ""
                    return "pgr_" + String(repeating: value.grants == 1 ? "a" : "b", count: 64)
                }
                return response(request, 200, ["grant": grant, "expires_in_seconds": 60])
            case "/hosted/v1/provisioning/complete":
                let completion = state.withValue { value in value.completions += 1; return value.completions }
                return completion == 1 ? response(request, 400, ["error": "grant_expired"]) : response(request, 200, completionResponse(for: request, vaultID: state.value.vaultID))
            default:
                return .init(status: 503, url: request.url, body: Data())
            }
        }
        let view = try await client.prepareVault(passphrase: validPassphrase)
        state.withValue { $0.vaultID = view.vaultId }
        _ = try await client.completeVault(passphrase: validPassphrase)
        #expect(state.value.grants == 2)
        #expect(state.value.completions == 2)
    }

    @Test func expiredGrantWithOwnedVaultFailsClosed() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        let accountId = UUID().uuidString.lowercased()
        let vaultID = UUID().uuidString.lowercased()
        try installSession(accountID: accountId, in: store)
        let state = Locked((accountRequests: 0, completionAttempted: false, operationID: ""))
        transport.setReply { request in
            switch request.url.path {
            case "/hosted/v1/account":
                let requestCount = state.withValue { value in value.accountRequests += 1; return value.accountRequests }
                return response(request, 200, ["account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write", "vault_id": requestCount == 1 ? NSNull() : vaultID, "operation_id": state.value.operationID.isEmpty ? NSNull() : state.value.operationID])
            case "/hosted/v1/provisioning":
                state.withValue { $0.operationID = requestJSON(request)["operation_id"] as? String ?? "" }
                return response(request, 200, ["grant": "pgr_" + String(repeating: "a", count: 64), "expires_in_seconds": 60])
            case "/hosted/v1/provisioning/complete":
                state.withValue { $0.completionAttempted = true }
                return response(request, 400, ["error": "grant_expired"])
            default:
                return .init(status: 503, url: request.url, body: Data())
            }
        }
        _ = try await client.prepareVault(passphrase: validPassphrase)
        await #expect(throws: HostedAccountClientError.self) { try await client.completeVault(passphrase: validPassphrase) }
        #expect(state.value.completionAttempted)
    }

    @Test func lateAuthAfterSignoutFails() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        let prepareInProgress = Locked(false)
        transport.setReply { r in
            if r.url.path.contains("/account") && !prepareInProgress.value {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            } else if r.url.path.contains("/provisioning") && r.method == "POST" && !r.url.path.contains("/complete") {
                // During prepare, session might be cleared
                prepareInProgress.withValue { $0 = true }
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: ["grant": "grant"]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        _ = try await client.prepareVault(passphrase: validPassphrase)
        let checkpoint = store.value("hosted-provisioning:https://peppy.pro")
        #expect(checkpoint != nil)
    }

    @Test func alreadyCompletedAccountPreservesOriginalGrant() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let accountId = UUID().uuidString.lowercased()
        let vaultID = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        let session = Stored(accountID: accountId, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
        
        let grantRequestCount = Locked(0)
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": accountId, "classification": "new", "entitlement": "active", "access": "read_write",
                    "vault_id": vaultID, "operation_id": UUID().uuidString.lowercased()
                ]))
            } else if r.url.path.contains("/provisioning") && r.method == "POST" && !r.url.path.contains("/complete") {
                let count = grantRequestCount.withValue { value in
                    value += 1
                    return value
                }
                if count == 1 { return .init(status: 200, url: r.url, body: Data()) }
                return .init(status: 403, url: r.url, body: Data())
            } else if r.url.path.contains("/complete") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: ["credential": "test"]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        await #expect(throws: HostedAccountClientError.self) { try await client.prepareVault(passphrase: validPassphrase) }
        #expect(grantRequestCount.value == 0)
    }

    @Test func differentAccountDoesNotReplayGrant() async throws {
        let store = InMemorySecureStore()
        let transport = HostedTransport()
        
        let account1 = UUID().uuidString.lowercased()
        let account2 = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        
        let session1 = Stored(accountID: account1, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session1))
        
        let client = try HostedAccountClient(transport: transport, secureStore: store)
        
        let completeAttempted = Locked(false)
        transport.setReply { r in
            if r.url.path.contains("/account") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: [
                    "account_id": account2, "classification": "new", "entitlement": "active", "access": "read_write"
                ]))
            } else if r.url.path.contains("/provisioning") && r.method == "POST" && !r.url.path.contains("/complete") {
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: ["grant": "grant"]))
            } else if r.url.path.contains("/complete") {
                completeAttempted.withValue { $0 = true }
                return .init(status: 200, url: r.url, body: try! JSONSerialization.data(withJSONObject: ["credential": "test"]))
            }
            return .init(status: 503, url: r.url, body: Data())
        }
        
        await #expect(throws: (any Error).self) { try await client.prepareVault(passphrase: validPassphrase) }
        #expect(!completeAttempted.value)
    }
}

private struct Stored: Codable { let accountID: String; let bearer: String; let expiresAt: Date }
private let validPassphrase = "alpha bravo charlie delta echo foxtrot"

private func installSession(accountID: String, in store: InMemorySecureStore) throws {
    let session = Stored(accountID: accountID, bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: Date(timeIntervalSinceNow: 300))
    try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(session))
}

private func requestJSON(_ request: HTTPRequest) -> [String: Any] {
    (request.body.flatMap { try? JSONSerialization.jsonObject(with: $0) }) as? [String: Any] ?? [:]
}

private func response(_ request: HTTPRequest, _ status: Int, _ object: [String: Any]) -> HTTPResponse {
    .init(status: status, url: request.url, body: try! JSONSerialization.data(withJSONObject: object))
}

private func completionResponse(for request: HTTPRequest, vaultID: String) -> [String: Any] {
    let body = requestJSON(request)
    return ["operation_id": body["operation_id"]!, "vault_id": vaultID, "device_id": body["device_id"]!, "already_provisioned": false]
}

private final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: Value

    init(_ value: Value) { storage = value }
    var value: Value { lock.withLock { storage } }
    func withValue<Result>(_ update: (inout Value) -> Result) -> Result { lock.withLock { update(&storage) } }
}

private final class HostedTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var reply: @Sendable (HTTPRequest) -> HTTPResponse
    
    init(_ reply: @escaping @Sendable (HTTPRequest) -> HTTPResponse = { r in .init(status: 503, url: r.url, body: Data()) }) {
        self.reply = reply
    }

    func setReply(_ reply: @escaping @Sendable (HTTPRequest) -> HTTPResponse) {
        lock.withLock { self.reply = reply }
    }
    
    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let reply = lock.withLock { self.reply }
        return reply(request)
    }
}
