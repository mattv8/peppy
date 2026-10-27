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

    @Test func userInteractionRenewsLocallyExpiredSession() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: accountID, bearer: bearer, expiresAt: .distantPast)))
        let requests = Locked([HTTPRequest]())
        let transport = HostedTransport { request in
            requests.withValue { $0.append(request) }
            return response(request, 200, ["account_id": accountID, "expires_in_seconds": 2_592_000])
        }
        let client = try HostedAccountClient(transport: transport, secureStore: store)

        try await client.renewSessionOnUserInteraction()

        let request = try #require(requests.value.first)
        #expect(request.method == "POST")
        #expect(request.url.path == "/hosted/v1/auth/session/renew")
        #expect(request.headers["Authorization"] == "Bearer \(bearer)")
        let renewed = try #require(store.value("hosted-account-session:https://peppy.pro").flatMap { try? JSONDecoder().decode(Stored.self, from: $0) })
        #expect(renewed.expiresAt > Date())
    }

    @Test func renewalUnauthorizedDeletesOnlyObservedSession() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        let original = Stored(accountID: accountID, bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: .distantPast)
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(original))
        try store.upsert("hosted-provisioning:https://peppy.pro", Data("checkpoint".utf8))
        let replacement = Stored(accountID: UUID().uuidString.lowercased(), bearer: "pst_" + String(repeating: "b", count: 64), expiresAt: Date(timeIntervalSinceNow: 300))
        let transport = HostedTransport { request in
            try! store.upsert("hosted-account-session:https://peppy.pro", try! JSONEncoder().encode(replacement))
            return .init(status: 401, url: request.url, body: Data())
        }
        let client = try HostedAccountClient(transport: transport, secureStore: store)

        await #expect(throws: CancellationError.self) { try await client.renewSessionOnUserInteraction() }

        let saved = try #require(store.value("hosted-account-session:https://peppy.pro").flatMap { try? JSONDecoder().decode(Stored.self, from: $0) })
        #expect(saved.bearer == replacement.bearer)
        #expect(store.value("hosted-provisioning:https://peppy.pro") != nil)
    }

    @Test func userInteractionDoesNotRenewBeforeThrottleWindow() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: accountID, bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: Date(timeIntervalSinceNow: 2_592_000))))
        let requests = Locked(0)
        let transport = HostedTransport { request in
            requests.withValue { $0 += 1 }
            return .init(status: 503, url: request.url, body: Data())
        }
        let client = try HostedAccountClient(transport: transport, secureStore: store)

        try await client.renewSessionOnUserInteraction()

        #expect(requests.value == 0)
    }

    @Test func concurrentUserInteractionsShareOneRenewal() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: accountID, bearer: "pst_" + String(repeating: "a", count: 64), expiresAt: .distantPast)))
        let requests = Locked(0)
        let transport = HostedTransport { request in
            requests.withValue { $0 += 1 }
            return response(request, 200, ["account_id": accountID, "expires_in_seconds": 2_592_000])
        }
        let client = try HostedAccountClient(transport: SlowTransport(inner: transport), secureStore: store)

        async let first: Void = client.renewSessionOnUserInteraction()
        async let second: Void = client.renewSessionOnUserInteraction()
        try await first
        try await second

        #expect(requests.value == 1)
    }

    @Test func delayedRenewalCannotRestoreSessionAfterOtherClientSignsOut() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        let bearer = "pst_" + String(repeating: "a", count: 64)
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: accountID, bearer: bearer, expiresAt: .distantPast)))
        let gate = RenewalGate()
        let transport = DelayedRenewalTransport(gate: gate) { request in
            response(request, 200, ["account_id": accountID, "expires_in_seconds": 2_592_000])
        }
        let renewingClient = try HostedAccountClient(transport: transport, secureStore: store)
        let signingOutClient = try HostedAccountClient(transport: transport, secureStore: store)

        let renewal = Task { try await renewingClient.renewSessionOnUserInteraction() }
        await gate.waitForRenewal()
        do {
            try await signingOutClient.signOut()
            await gate.releaseRenewal()
        } catch {
            await gate.releaseRenewal()
            throw error
        }

        await #expect(throws: CancellationError.self) { try await renewal.value }
        #expect(store.value("hosted-account-session:https://peppy.pro") == nil)
    }

    @Test func delayedRenewalCannotRestoreSessionAfterOtherClientLogsIn() async throws {
        let store = InMemorySecureStore()
        let accountID = UUID().uuidString.lowercased()
        let oldBearer = "pst_" + String(repeating: "a", count: 64)
        let newBearer = "pst_" + String(repeating: "b", count: 64)
        try store.upsert("hosted-account-session:https://peppy.pro", try JSONEncoder().encode(Stored(accountID: accountID, bearer: oldBearer, expiresAt: .distantPast)))
        let gate = RenewalGate()
        let transport = DelayedRenewalTransport(gate: gate) { request in
            switch request.url.path {
            case "/hosted/v1/auth/session/renew":
                return response(request, 200, ["account_id": accountID, "expires_in_seconds": 2_592_000])
            case "/hosted/v1/auth/session":
                return response(request, 200, ["account_id": accountID, "session_token": newBearer, "expires_in_seconds": 2_592_000])
            case "/hosted/v1/account":
                return response(request, 200, ["account_id": accountID, "classification": "new", "entitlement": "active", "access": "read_write"])
            default:
                return .init(status: 503, url: request.url, body: Data())
            }
        }
        let renewingClient = try HostedAccountClient(transport: transport, secureStore: store)
        let loginClient = try HostedAccountClient(transport: transport, secureStore: store)

        let renewal = Task { try await renewingClient.renewSessionOnUserInteraction() }
        await gate.waitForRenewal()
        do {
            _ = try await loginClient.finishGoogleSignIn(attemptID: UUID().uuidString.lowercased(), idToken: "test-provider-token")
            await gate.releaseRenewal()
        } catch {
            await gate.releaseRenewal()
            throw error
        }

        await #expect(throws: CancellationError.self) { try await renewal.value }
        let saved = try #require(store.value("hosted-account-session:https://peppy.pro").flatMap { try? JSONDecoder().decode(Stored.self, from: $0) })
        #expect(saved.bearer == newBearer)
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

    @Test func signOutDeletesCorruptSession() async throws {
        let store = InMemorySecureStore()
        try store.upsert("hosted-account-session:https://peppy.pro", Data("corrupt".utf8))
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)

        try await client.signOut()

        #expect(store.value("hosted-account-session:https://peppy.pro") == nil)
    }

    @Test func signOutWithoutSessionDoesNotContactServer() async throws {
        let requests = Locked(0)
        let transport = HostedTransport { request in
            requests.withValue { $0 += 1 }
            return .init(status: 503, url: request.url, body: Data())
        }
        let client = try HostedAccountClient(transport: transport, secureStore: InMemorySecureStore())

        try await client.signOut()

        #expect(requests.value == 0)
    }

    @Test func signOutDeletesUnreadableSession() async throws {
        let store = SignOutStore(readFailure: true)
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)

        try await client.signOut()

        #expect(store.deleteCalls == 1)
    }

    @Test func userInteractionBacksOffBeforeRereadingUnreadableSession() async throws {
        let store = SignOutStore(readFailure: true)
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)

        await #expect(throws: ClientError.self) { try await client.renewSessionOnUserInteraction() }
        try await client.renewSessionOnUserInteraction()

        #expect(store.readCalls == 1)
    }

    @Test func signOutPropagatesSessionDeleteFailure() async throws {
        let store = SignOutStore(deleteFailure: true)
        let client = try HostedAccountClient(transport: HostedTransport(), secureStore: store)

        await #expect(throws: ClientError.self) { try await client.signOut() }

        #expect(store.deleteCalls == 1)
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

private actor RenewalGate {
    private var started = false
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var releaseWaiter: CheckedContinuation<Void, Never>?

    func waitForRenewal() async {
        if started { return }
        await withCheckedContinuation { startWaiter = $0 }
    }

    func holdRenewal() async {
        started = true
        startWaiter?.resume()
        startWaiter = nil
        await withCheckedContinuation { releaseWaiter = $0 }
    }

    func releaseRenewal() {
        releaseWaiter?.resume()
        releaseWaiter = nil
    }
}

private final class DelayedRenewalTransport: HTTPTransport, @unchecked Sendable {
    let gate: RenewalGate
    let reply: @Sendable (HTTPRequest) -> HTTPResponse

    init(gate: RenewalGate, reply: @escaping @Sendable (HTTPRequest) -> HTTPResponse) {
        self.gate = gate
        self.reply = reply
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        if request.url.path == "/hosted/v1/auth/session/renew" { await gate.holdRenewal() }
        return reply(request)
    }
}

private final class SignOutStore: SecureStore, @unchecked Sendable {
    private let lock = NSLock()
    private let readFailure: Bool
    private let deleteFailure: Bool
    private(set) var readCalls = 0
    private(set) var deleteCalls = 0

    init(readFailure: Bool = false, deleteFailure: Bool = false) {
        self.readFailure = readFailure
        self.deleteFailure = deleteFailure
    }

    func read(_ account: String) throws(ClientError) -> Data? {
        lock.withLock { readCalls += 1 }
        if readFailure { throw .secureStorage(status: -1) }
        return nil
    }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool { true }
    func upsert(_ account: String, _ data: Data) throws(ClientError) {}
    func delete(_ account: String) throws(ClientError) {
        lock.withLock { deleteCalls += 1 }
        if deleteFailure { throw .secureStorage(status: -1) }
    }
}
