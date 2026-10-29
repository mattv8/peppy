import Foundation
import Testing
@testable import PeppyNative
import PeppyBindings

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

    // MARK: - Q07 recovery: each test injects the fault, cancellation, stale response or replay it names.

    @Test func storeFailureBeforeGrantPreservesCheckpoint() async throws {
        let f = try RecoveryFixture()
        // A checkpoint write that fails during prepare leaves no checkpoint and makes no remote write.
        f.store.failWrites(to: checkpointKey)
        await #expect(throws: injectedStoreFailure) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.server.posts.isEmpty)

        f.store.failWrites(to: nil)
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        let prepared = try #require(f.store.value(checkpointKey))
        #expect(checkpointField(prepared, "grant") == nil)

        // The grant arrives but cannot be persisted: it is never spent and the checkpoint stays as prepared.
        f.store.failWrites(to: checkpointKey)
        await #expect(throws: injectedStoreFailure) { try await f.client.completeVault(passphrase: validPassphrase) }
        #expect(f.server.grantCount == 1)
        #expect(f.server.completeBodies.isEmpty)
        #expect(f.store.value(checkpointKey) == prepared)

        // Once storage recovers, the same operation gets a fresh grant and completes.
        f.store.failWrites(to: nil)
        let credential = try await f.client.completeVault(passphrase: validPassphrase)
        #expect(f.server.grantOperationIDs == [view.operationId, view.operationId])
        #expect(requestField(try #require(f.server.completeBodies.first), "grant") == grantToken(2))
        #expect(credentialField(credential, "vaultId") == view.vaultId)
    }

    @Test func lostCompletionResponseReplayWithSameToken() async throws {
        let f = try RecoveryFixture()
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        f.server.loseNextCompleteResponse()
        await #expect(throws: lostResponse) { try await f.client.completeVault(passphrase: validPassphrase) }
        let afterLoss = try #require(f.store.value(checkpointKey))
        #expect(checkpointField(afterLoss, "grant") == grantToken(1))

        let credential = try await f.client.completeVault(passphrase: validPassphrase)
        let bodies = f.server.completeBodies
        #expect(f.server.grantCount == 1)
        #expect(bodies.count == 2)
        #expect(bodies.first == bodies.last)
        #expect(requestField(try #require(bodies.last), "grant") == grantToken(1))
        #expect(credentialField(credential, "vaultId") == view.vaultId)
        #expect(credentialField(credential, "deviceId") == view.deviceId)
        #expect(f.store.value(checkpointKey) == afterLoss)
    }

    @Test func canceledPrepareVaultCannotMutateCheckpoint() async throws {
        let f = try RecoveryFixture()
        let gate = Gate()
        f.server.pause("GET /hosted/v1/account", at: gate)
        let prepare = Task { _ = try await f.client.prepareVault(passphrase: validPassphrase) }
        await gate.waitUntilEntered()
        prepare.cancel()
        await gate.release()
        await #expect(throws: CancellationError.self) { try await prepare.value }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        #expect(f.server.requests == ["GET /hosted/v1/account"])

        // Nothing left behind blocks a later attempt with a different phrase.
        let other = generateHostedPassphrase()
        let view = try await f.client.prepareVault(passphrase: other)
        #expect(checkpointField(f.store.value(checkpointKey), "operation_id") == view.operationId)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(other.utf8))
    }

    @Test func stalePrepareResponseDoesNotMutateCheckpoint() async throws {
        let f = try RecoveryFixture()
        let accountB = UUID().uuidString.lowercased()
        f.server.acceptNextLogin(account: accountB, bearer: bearerToken("b"))
        let gate = Gate()
        f.server.pause("GET /hosted/v1/account", at: gate)
        let stale = Task { _ = try await f.client.prepareVault(passphrase: validPassphrase) }
        await gate.waitUntilEntered()
        // A newer login completes while account A's response is still in flight.
        let account = try await f.client.finishGoogleSignIn(attemptID: UUID().uuidString.lowercased(), idToken: "provider-token")
        #expect(account.accountId == accountB)
        await gate.release()
        await #expect(throws: CancellationError.self) { try await stale.value }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        #expect(f.store.value(pendingKey(accountB)) == nil)
        #expect(f.server.posts == ["POST /hosted/v1/auth/session"])

        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        #expect(view.accountId == accountB)
        #expect(checkpointField(f.store.value(checkpointKey), "account_id") == accountB)
    }

    @Test func lateAuthAfterSignoutFails() async throws {
        let f = try RecoveryFixture(signedIn: false)
        f.server.acceptNextLogin(account: f.accountID, bearer: bearerToken("a"))

        // Sign-out while the session exchange is in flight: the late session is never stored.
        let exchange = Gate()
        f.server.pause("POST /hosted/v1/auth/session", at: exchange)
        let first = Task { _ = try await f.client.finishGoogleSignIn(attemptID: UUID().uuidString.lowercased(), idToken: "provider-token") }
        await exchange.waitUntilEntered()
        try await f.client.signOut()
        await exchange.release()
        await #expect(throws: CancellationError.self) { try await first.value }
        #expect(f.store.value(sessionKey) == nil)
        #expect(f.server.requests == ["POST /hosted/v1/auth/session"])

        // Sign-out after the session was stored, while the account read is in flight: it stays signed out.
        let accountRead = Gate()
        f.server.pause("GET /hosted/v1/account", at: accountRead)
        let second = Task { _ = try await f.client.finishGoogleSignIn(attemptID: UUID().uuidString.lowercased(), idToken: "provider-token") }
        await accountRead.waitUntilEntered()
        #expect(f.store.value(sessionKey) != nil)
        try await f.client.signOut()
        await accountRead.release()
        await #expect(throws: CancellationError.self) { try await second.value }
        #expect(f.store.value(sessionKey) == nil)
        #expect(f.server.requests.last == "DELETE /hosted/v1/auth/session")

        let requestCount = f.server.requests.count
        await #expect(throws: HostedAccountClientError.sessionExpired) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.server.requests.count == requestCount)
        #expect(f.store.value(checkpointKey) == nil)
    }

    @Test func alreadyCompletedAccountPreservesOriginalGrant() async throws {
        let f = try RecoveryFixture()
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        // The server commits the vault but rejects this completion, so the account now owns a vault.
        f.server.rejectNextComplete(status: 409, committing: true)
        await #expect(throws: HostedAccountClientError.unavailable) { try await f.client.completeVault(passphrase: validPassphrase) }
        #expect(f.server.grantCount == 1)
        #expect(f.server.completeBodies.count == 1)
        #expect(f.server.requests.filter { $0 == "GET /hosted/v1/account" }.count == 2)
        let retained = try #require(f.store.value(checkpointKey))
        #expect(checkpointField(retained, "grant") == grantToken(1))

        // A later replay presents the original grant and receives the already-provisioned credential.
        let credential = try await f.client.completeVault(passphrase: validPassphrase)
        let bodies = f.server.completeBodies
        #expect(f.server.grantCount == 1)
        #expect(bodies.count == 2)
        #expect(bodies.first == bodies.last)
        #expect(requestField(try #require(bodies.last), "grant") == grantToken(1))
        #expect(credentialField(credential, "vaultId") == view.vaultId)
        #expect(f.store.value(checkpointKey) == retained)
    }

    @Test func differentAccountDoesNotReplayGrant() async throws {
        let f = try RecoveryFixture()
        _ = try await f.client.prepareVault(passphrase: validPassphrase)
        f.server.loseNextCompleteResponse()
        await #expect(throws: lostResponse) { try await f.client.completeVault(passphrase: validPassphrase) }
        let checkpointA = try #require(f.store.value(checkpointKey))
        #expect(checkpointField(checkpointA, "grant") == grantToken(1))

        let accountB = UUID().uuidString.lowercased()
        try f.signIn(accountB, bearer: bearerToken("b"))
        let postCount = f.server.posts.count
        await #expect(throws: HostedAccountClientError.wrongAccount) { try await f.client.completeVault(passphrase: validPassphrase) }
        await #expect(throws: HostedAccountClientError.wrongAccount) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.server.posts.count == postCount)
        #expect(f.server.completeBodies.count == 1)
        #expect(f.store.value(checkpointKey) == checkpointA)
        #expect(f.store.value(pendingKey(accountB)) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
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

    // MARK: - H03 pending secret. Binding only after a verified unlock lives in the app target
    // (HostedEnrollmentView) and needs the iOS SDK; it is not exercised by this host suite.

    @Test func pendingSecretIsStoredBeforeCheckpoint() async throws {
        let f = try RecoveryFixture()
        _ = try await f.client.prepareVault(passphrase: validPassphrase)
        let writes = f.store.writeAttempts
        let pending = try #require(writes.firstIndex(of: pendingKey(f.accountID)))
        let checkpoint = try #require(writes.firstIndex(of: checkpointKey))
        #expect(pending < checkpoint)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
    }

    @Test func failedPendingWriteLeavesNoCheckpointAndNoPost() async throws {
        let f = try RecoveryFixture()
        f.store.failWrites(to: pendingKey(f.accountID))
        await #expect(throws: injectedStoreFailure) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.server.posts.isEmpty)

        // Resuming a checkpoint without a stored secret (older install) cannot reach the server if the secret cannot be kept.
        f.store.failWrites(to: nil)
        _ = try await f.client.prepareVault(passphrase: validPassphrase)
        let checkpoint = try #require(f.store.value(checkpointKey))
        try f.store.delete(pendingKey(f.accountID))
        f.store.failWrites(to: pendingKey(f.accountID))
        await #expect(throws: injectedStoreFailure) { try await f.client.completeVault(passphrase: validPassphrase) }
        #expect(f.server.posts.isEmpty)
        #expect(f.store.value(checkpointKey) == checkpoint)
    }

    @Test func wrongResumePhraseDoesNotPoisonPendingSecret() async throws {
        let f = try RecoveryFixture()
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        try f.store.delete(pendingKey(f.accountID))
        await #expect(throws: HostedAccountClientError.wrongPassphrase) { try await f.client.prepareVault(passphrase: wrongPassphrase) }
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        await #expect(throws: HostedAccountClientError.wrongPassphrase) { try await f.client.completeVault(passphrase: wrongPassphrase) }
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        #expect(f.server.posts.isEmpty)

        let resumed = try await f.client.prepareVault(passphrase: validPassphrase)
        #expect(resumed.operationId == view.operationId)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
    }

    @Test func restartReusesExactOperationAndPhrase() async throws {
        let f = try RecoveryFixture()
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        let restarted = try f.restartedClient()
        let resumed = try await restarted.prepareVault(passphrase: validPassphrase)
        #expect(resumed.operationId == view.operationId)
        #expect(resumed.vaultId == view.vaultId)
        #expect(resumed.deviceId == view.deviceId)

        let credential = try await restarted.completeVault(passphrase: validPassphrase)
        #expect(f.server.grantOperationIDs == [view.operationId])
        #expect(credentialField(credential, "deviceId") == view.deviceId)
        // The pending secret survives completion until a verified binding consumes it.
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
    }

    @Test func accountAPendingSecretCannotBindAccountB() async throws {
        let f = try RecoveryFixture()
        _ = try await f.client.prepareVault(passphrase: validPassphrase)
        let checkpointA = try #require(f.store.value(checkpointKey))
        let accountB = UUID().uuidString.lowercased()
        try f.signIn(accountB, bearer: bearerToken("b"))
        let identityB = EnrollmentIdentity(origin: "https://peppy.pro", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased())

        await #expect(throws: HostedAccountClientError.wrongAccount) { try await f.client.prepareVault(passphrase: validPassphrase) }
        await #expect(throws: HostedAccountClientError.wrongAccount) { try await f.client.acknowledgeEnrollment(identity: identityB, passphrase: validPassphrase) }
        #expect(f.store.value(pendingKey(accountB)) == nil)

        // B binds only what B entered; A's pending secret is neither used nor consumed.
        let secretB = generateHostedPassphrase()
        try await f.client.bindVerifiedEnrollmentSecret(secretB, identity: identityB)
        #expect(try VaultSecretStore(secure: f.store).read(identityB) == secretB)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
        #expect(f.store.value(checkpointKey) == checkpointA)
    }

    @Test func conflictingPendingSecretFailsClosed() async throws {
        let f = try RecoveryFixture()
        let conflicting = Data(generateHostedPassphrase().utf8)
        try f.store.upsert(pendingKey(f.accountID), conflicting)
        await #expect(throws: HostedAccountClientError.wrongPassphrase) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == conflicting)

        try f.store.delete(pendingKey(f.accountID))
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        let checkpoint = try #require(f.store.value(checkpointKey))
        try f.store.upsert(pendingKey(f.accountID), conflicting)
        await #expect(throws: HostedAccountClientError.wrongPassphrase) { try await f.client.completeVault(passphrase: validPassphrase) }
        #expect(f.server.posts.isEmpty)
        #expect(f.store.value(checkpointKey) == checkpoint)

        let identity = EnrollmentIdentity(origin: "https://peppy.pro", vaultId: view.vaultId, deviceId: view.deviceId)
        await #expect(throws: HostedAccountClientError.wrongPassphrase) { try await f.client.bindVerifiedEnrollmentSecret(validPassphrase, identity: identity) }
        #expect(try VaultSecretStore(secure: f.store).read(identity) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == conflicting)
    }

    @Test func bindThenAcknowledgeConsumesPendingAndCheckpoint() async throws {
        let f = try RecoveryFixture()
        let view = try await f.client.prepareVault(passphrase: validPassphrase)
        let identity = try enrolledIdentity(try await f.client.completeVault(passphrase: validPassphrase))
        #expect(identity.vaultId == view.vaultId && identity.deviceId == view.deviceId)

        try await f.client.bindVerifiedEnrollmentSecret(validPassphrase, identity: identity)
        #expect(f.store.value(pendingKey(f.accountID)) == nil, "the verified binding consumes the pending copy")
        try await f.client.acknowledgeEnrollment(identity: identity, passphrase: validPassphrase)
        #expect(try VaultSecretStore(secure: f.store).read(identity) == validPassphrase)
        #expect(f.store.value(pendingKey(f.accountID)) == nil, "acknowledgement never recreates the pending secret")
        #expect(f.store.value(checkpointKey) == nil)

        // After that vault is deleted, the same account provisions a new vault with a new phrase.
        f.server.deleteVault(account: f.accountID)
        let next = generateHostedPassphrase()
        let fresh = try await f.client.prepareVault(passphrase: next)
        #expect(fresh.vaultId != view.vaultId)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(next.utf8))
    }

    @Test func acknowledgementWithoutTheVerifiedBindingKeepsRecovery() async throws {
        let f = try RecoveryFixture()
        _ = try await f.client.prepareVault(passphrase: validPassphrase)
        let identity = try enrolledIdentity(try await f.client.completeVault(passphrase: validPassphrase))
        let checkpoint = try #require(f.store.value(checkpointKey))
        let pending = Data(validPassphrase.utf8)
        let bindingKey = identity.account("vault-secret-v2")

        // No binding yet (unlock or bind never finished): fail closed, keep everything.
        await #expect(throws: HostedAccountClientError.unavailable) {
            try await f.client.acknowledgeEnrollment(identity: identity, passphrase: validPassphrase)
        }
        #expect(f.store.value(checkpointKey) == checkpoint)
        #expect(f.store.value(pendingKey(f.accountID)) == pending)

        // A conflicting binding is never accepted as proof.
        try f.store.upsert(bindingKey, Data(wrongPassphrase.utf8))
        await #expect(throws: HostedAccountClientError.wrongPassphrase) {
            try await f.client.acknowledgeEnrollment(identity: identity, passphrase: validPassphrase)
        }
        #expect(f.store.value(checkpointKey) == checkpoint)
        #expect(f.store.value(pendingKey(f.accountID)) == pending)
        try f.store.delete(bindingKey)

        // A failed binding write keeps the pending secret and checkpoint for the retry.
        f.store.failWrites(to: bindingKey)
        await #expect(throws: HostedAccountClientError.unavailable) {
            try await f.client.bindVerifiedEnrollmentSecret(validPassphrase, identity: identity)
        }
        #expect(f.store.value(bindingKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == pending)
        #expect(f.store.value(checkpointKey) == checkpoint)

        // The retried bind succeeds; a failed checkpoint removal keeps the checkpoint, and the
        // retried acknowledgement finishes without recreating the pending secret.
        f.store.failWrites(to: nil)
        try await f.client.bindVerifiedEnrollmentSecret(validPassphrase, identity: identity)
        f.store.failDeletes(of: checkpointKey)
        await #expect(throws: injectedStoreFailure) {
            try await f.client.acknowledgeEnrollment(identity: identity, passphrase: validPassphrase)
        }
        #expect(f.store.value(checkpointKey) == checkpoint)
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        f.store.failDeletes(of: nil)
        try await f.client.acknowledgeEnrollment(identity: identity, passphrase: validPassphrase)
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == nil)
        #expect(f.store.value(bindingKey) == pending)
    }

    @Test func orphanedPendingSecretIsRecoverableForItsAccountOnly() async throws {
        let f = try RecoveryFixture()
        f.store.failWrites(to: checkpointKey)
        await #expect(throws: injectedStoreFailure) { try await f.client.prepareVault(passphrase: validPassphrase) }
        #expect(f.store.value(checkpointKey) == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
        f.store.failWrites(to: nil)

        // A relaunch has no checkpoint, but the account's protected phrase is still offered.
        let restarted = try f.restartedClient()
        #expect(try await restarted.candidatePendingSecret() == validPassphrase)
        await #expect(throws: HostedAccountClientError.wrongPassphrase) {
            try await restarted.prepareVault(passphrase: generateHostedPassphrase())
        }
        let recovered = try #require(try await restarted.candidatePendingSecret())
        let view = try await restarted.prepareVault(passphrase: recovered)
        #expect(f.store.value(checkpointKey) != nil)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
        let identity = try enrolledIdentity(try await restarted.completeVault(passphrase: recovered))
        #expect(identity.vaultId == view.vaultId)

        // A storage failure is surfaced, never treated as "no secret".
        f.store.failReads(of: pendingKey(f.accountID))
        await #expect(throws: injectedStoreFailure) { try await restarted.candidatePendingSecret() }
        f.store.failReads(of: nil)

        // Another account never sees A's secret.
        try f.signIn(UUID().uuidString.lowercased(), bearer: bearerToken("b"))
        #expect(try await restarted.candidatePendingSecret() == nil)
        #expect(f.store.value(pendingKey(f.accountID)) == Data(validPassphrase.utf8))
    }
}

private func enrolledIdentity(_ credential: Data) throws -> EnrollmentIdentity {
    EnrollmentIdentity(
        origin: "https://peppy.pro",
        vaultId: try #require(credentialField(credential, "vaultId")),
        deviceId: try #require(credentialField(credential, "deviceId"))
    )
}

private struct Stored: Codable { let accountID: String; let bearer: String; let expiresAt: Date }
private let validPassphrase = "alpha bravo charlie delta echo foxtrot"

private func installSession(accountID: String, bearer: String = "pst_" + String(repeating: "a", count: 64), in store: any SecureStore) throws {
    let session = Stored(accountID: accountID, bearer: bearer, expiresAt: Date(timeIntervalSinceNow: 300))
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

final class Locked<Value>: @unchecked Sendable {
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

// MARK: - Recovery fixtures

private let sessionKey = "hosted-account-session:https://peppy.pro"
private let checkpointKey = "hosted-provisioning:https://peppy.pro"
private let wrongPassphrase = "wrong phrase with sufficient length here"
private let injectedStoreFailure = ClientError.secureStorage(status: -25308)
private let lostResponse = ClientError.network(code: -1005)

private func pendingKey(_ accountID: String) -> String { "vault-secret-pending-v2:https://peppy.pro|hosted-account|\(accountID)" }
private func bearerToken(_ digit: Character) -> String { "pst_" + String(repeating: digit, count: 64) }
private func grantToken(_ issued: Int) -> String {
    let digits = Array("abcdef0123456789")
    return "pgr_" + String(repeating: digits[(issued - 1) % digits.count], count: 64)
}
private func jsonObject(_ data: Data?) -> [String: Any] {
    (data.flatMap { try? JSONSerialization.jsonObject(with: $0) }) as? [String: Any] ?? [:]
}
private func checkpointField(_ checkpoint: Data?, _ key: String) -> String? { jsonObject(checkpoint)[key] as? String }
private func credentialField(_ credential: Data, _ key: String) -> String? { jsonObject(credential)[key] as? String }
private func requestField(_ body: String, _ key: String) -> String? { jsonObject(Data(body.utf8))[key] as? String }

/// A signed-in client over one store and one fake hosted server; `restartedClient` models an app relaunch.
private struct RecoveryFixture: Sendable {
    let store = FaultingSecureStore()
    let server = HostedServer()
    let accountID = UUID().uuidString.lowercased()
    let client: HostedAccountClient

    init(signedIn: Bool = true) throws {
        client = try HostedAccountClient(transport: server, secureStore: store)
        if signedIn { try signIn(accountID, bearer: bearerToken("a")) }
    }

    func signIn(_ account: String, bearer: String) throws {
        server.register(account: account, bearer: bearer)
        try installSession(accountID: account, bearer: bearer, in: store)
    }

    func restartedClient() throws -> HostedAccountClient { try HostedAccountClient(transport: server, secureStore: store) }
}

/// In-memory secure store that records write attempts and can fail writes to one account.
private final class FaultingSecureStore: SecureStore, @unchecked Sendable {
    private let backing = InMemorySecureStore()
    private let state = Locked((failing: String?.none, failingDelete: String?.none, failingRead: String?.none, writes: [String]()))

    var writeAttempts: [String] { state.value.writes }
    func failWrites(to account: String?) { state.withValue { $0.failing = account } }
    func failDeletes(of account: String?) { state.withValue { $0.failingDelete = account } }
    func failReads(of account: String?) { state.withValue { $0.failingRead = account } }
    func value(_ account: String) -> Data? { backing.value(account) }

    func read(_ account: String) throws(ClientError) -> Data? {
        if state.value.failingRead == account { throw injectedStoreFailure }
        return try backing.read(account)
    }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        try recordWrite(account)
        return try backing.addIfAbsent(account, data)
    }
    func upsert(_ account: String, _ data: Data) throws(ClientError) {
        try recordWrite(account)
        try backing.upsert(account, data)
    }
    func delete(_ account: String) throws(ClientError) {
        if state.value.failingDelete == account { throw injectedStoreFailure }
        try backing.delete(account)
    }

    private func recordWrite(_ account: String) throws(ClientError) {
        let fails = state.withValue { state -> Bool in
            guard state.failing != account else { return true }
            state.writes.append(account)
            return false
        }
        if fails { throw injectedStoreFailure }
    }
}

/// Holds one request until the test releases it, and lets the test wait until that request arrived.
private actor Gate {
    private var entered = false
    private var released = false
    private var enteredWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func arriveAndWait() async {
        entered = true
        enteredWaiters.forEach { $0.resume() }
        enteredWaiters = []
        guard !released else { return }
        await withCheckedContinuation { releaseWaiters.append($0) }
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { enteredWaiters.append($0) }
    }

    func release() {
        released = true
        releaseWaiters.forEach { $0.resume() }
        releaseWaiters = []
    }
}

/// Stand-in for the hosted account and provisioning routes, mirroring the Android test server.
private final class HostedServer: HTTPTransport, @unchecked Sendable {
    private struct State {
        var accountsByBearer: [String: String] = [:]
        var nextLogin: (account: String, bearer: String)?
        var vaultByAccount: [String: String] = [:]
        var grantOperationIDs: [String] = []
        var completeBodies: [String] = []
        var completeRejection: (status: Int, committing: Bool)?
        var loseNextComplete = false
        var requests: [String] = []
        var pauses: [String: Gate] = [:]
    }
    private let state = Locked(State())

    var requests: [String] { state.value.requests }
    var posts: [String] { requests.filter { $0.hasPrefix("POST ") } }
    var grantCount: Int { state.value.grantOperationIDs.count }
    var grantOperationIDs: [String] { state.value.grantOperationIDs }
    var completeBodies: [String] { state.value.completeBodies }

    func register(account: String, bearer: String) { state.withValue { $0.accountsByBearer[bearer] = account } }
    func acceptNextLogin(account: String, bearer: String) { state.withValue { $0.nextLogin = (account, bearer) } }
    func pause(_ route: String, at gate: Gate) { state.withValue { $0.pauses[route] = gate } }
    func loseNextCompleteResponse() { state.withValue { $0.loseNextComplete = true } }
    func rejectNextComplete(status: Int, committing: Bool) { state.withValue { $0.completeRejection = (status, committing) } }
    func deleteVault(account: String) { state.withValue { _ = $0.vaultByAccount.removeValue(forKey: account) } }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let route = "\(request.method) \(request.url.path)"
        let gate = state.withValue { state -> Gate? in
            state.requests.append(route)
            return state.pauses.removeValue(forKey: route)
        }
        if let gate { await gate.arriveAndWait() }
        let bearer = request.headers["Authorization"].map { String($0.dropFirst("Bearer ".count)) }
        let account = bearer.flatMap { bearer in state.value.accountsByBearer[bearer] }
        switch route {
        case "POST /hosted/v1/auth/session":
            guard let login = state.value.nextLogin else { return response(request, 401, ["error": "unauthorized"]) }
            register(account: login.account, bearer: login.bearer)
            return response(request, 200, ["account_id": login.account, "session_token": login.bearer, "expires_in_seconds": 3600])
        case "DELETE /hosted/v1/auth/session":
            return .init(status: 204, url: request.url, body: Data())
        case _ where account == nil:
            return response(request, 401, ["error": "unauthorized"])
        case "GET /hosted/v1/account":
            let vault = state.value.vaultByAccount[account!]
            return response(request, 200, ["account_id": account!, "classification": vault == nil ? "new" : "existing", "entitlement": "active", "access": "read_write", "vault_id": vault ?? NSNull(), "operation_id": NSNull()])
        case "POST /hosted/v1/provisioning":
            let operationID = requestJSON(request)["operation_id"] as? String ?? ""
            let issued = state.withValue { state in state.grantOperationIDs.append(operationID); return state.grantOperationIDs.count }
            return response(request, 200, ["grant": grantToken(issued), "expires_in_seconds": 600])
        case "POST /hosted/v1/provisioning/complete":
            return try complete(request, account: account!)
        default:
            return .init(status: 503, url: request.url, body: Data())
        }
    }

    private func complete(_ request: HTTPRequest, account: String) throws -> HTTPResponse {
        let body = requestJSON(request)
        let vaultID = (body["public_key_profile"] as? [String: Any])?["vault_id"] as? String ?? ""
        let outcome = state.withValue { state -> (rejection: Int?, lose: Bool, already: Bool) in
            state.completeBodies.append(String(decoding: request.body ?? Data(), as: UTF8.self))
            let already = state.vaultByAccount[account] != nil
            if let rejection = state.completeRejection {
                state.completeRejection = nil
                if rejection.committing { state.vaultByAccount[account] = vaultID }
                return (rejection.status, false, already)
            }
            state.vaultByAccount[account] = vaultID
            defer { state.loseNextComplete = false }
            return (nil, state.loseNextComplete, already)
        }
        if let status = outcome.rejection { return response(request, status, ["error": "rejected"]) }
        if outcome.lose { throw lostResponse }
        return response(request, 200, ["operation_id": body["operation_id"] ?? NSNull(), "vault_id": vaultID, "device_id": body["device_id"] ?? NSNull(), "already_provisioned": outcome.already])
    }
}
