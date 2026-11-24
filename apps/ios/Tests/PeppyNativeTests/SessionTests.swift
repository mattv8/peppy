import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

/// Import, Keychain layout, unlock and key-cache restore through a real SQLCipher core.
@Suite struct SessionTests {
    let passphrase = "io synthetic vault passphrase"

    @Test func closedClientRejectsNativeRcsSendFactory() throws {
        let config = NativeOpenConfig(
            databasePath: temporaryDirectory().appendingPathComponent("closed.db").path,
            vaultId: UUID().uuidString,
            deviceId: UUID().uuidString,
            databaseKey: Data(repeating: 7, count: 32)
        )
        let client = try openNativeClient(config: config)
        try client.dispose()
        #expect(throws: MobileBindingsError.Closed) {
            _ = try gatewayPrepareNativeRcsSend(client: client, commandId: UUID().uuidString, nowMs: 1_000)
        }
    }

    func session(_ server: FakeServer, store: any SecureStore, deviceId: String, directory: URL) -> NativeSession {
        NativeSession(
            secureStore: store,
            transport: DeviceScopedTransport(server: server, deviceId: deviceId),
            databaseDirectory: directory,
            allowLoopbackHTTP: false
        )
    }

    private static func cleanupReady(_ request: HTTPRequest) -> Bool? {
        guard request.url.path == "/v1/devices/self/retention-capability",
              let body = request.body,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return nil }
        return object["cleanup_ready"] as? Bool
    }

    @Test func v2ImportRefusesAStaleProfileBelowTheAnchorFloorAndPersistsNothing() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let stale = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())

        // The server's profile is epoch 1; a floor of 2 makes it stale.
        await #expect(throws: ClientError.core(.InvalidProfile)) {
            try await stale.importCredential(server.credential(deviceId: deviceId, anchor: (2, String(repeating: "aa", count: 32))))
        }
        #expect(store.accounts.isEmpty)
    }

    @Test func v2ImportAppliesTheAnchorBeforeAnyUnlock() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let anchored = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())

        // A fingerprint that is not the profile's, at its own epoch: the core must refuse the unlock.
        _ = try await anchored.importCredential(server.credential(deviceId: deviceId, anchor: (1, String(repeating: "aa", count: 32))))
        await #expect(throws: ClientError.core(.InvalidProfile)) { try await anchored.unlock(passphrase: passphrase) }
        await anchored.close()
    }

    @Test(arguments: [1, 2])
    func interruptedAnchoredImportRestoresTheFenceBeforeCachedKeys(floor: Int) async throws {
        let server = try FakeServer(passphrase: passphrase)
        let secure = RecoverySecureStore()
        let device = UUID().uuidString.lowercased()
        let directory = temporaryDirectory()
        let first = session(server, store: secure, deviceId: device, directory: directory)
        let identity = try #require(try await first.importCredential(server.credential(deviceId: device)).identity)
        _ = try await first.unlock(passphrase: passphrase)
        await first.close()

        // Keep the epoch-1 cache, but let verification advertise the imported floor.
        var vault = server.vault(deviceId: device)
        vault["key_epoch"] = floor
        let response = try JSONSerialization.data(withJSONObject: vault)
        server.mutate { $0.override = { request in
            request.url.path == "/v1/vault" ? HTTPResponse(status: 200, url: request.url, body: response) : nil
        } }
        let interrupted = session(server, store: secure, deviceId: device, directory: directory)
        secure.failingRead = identity.account("database-key")
        await #expect(throws: recoveryStoreFailure) {
            try await interrupted.importCredential(server.credential(deviceId: device, anchor: (floor, String(repeating: "aa", count: 32))))
        }
        let durable = try EnrollmentStore(secure: secure).record(identity)
        #expect(durable.epochFloor == UInt32(floor))
        #expect(try EnrollmentStore(secure: secure).activeIdentity() == identity)
        secure.failingRead = nil

        let restarted = session(server, store: secure, deviceId: device, directory: directory)
        let opened = try await restarted.open()
        #expect(opened.databaseOpen && !opened.keysUnlocked)
        #expect(opened.rejectedKeyCaches == 1)
        await #expect(throws: ClientError.core(.InvalidProfile)) { try await restarted.unlock(passphrase: passphrase) }
        // Neither an unanchored server refresh nor a v1 re-import may erase the floor.
        _ = try await restarted.refreshEnrollment()
        _ = try await restarted.importCredential(server.credential(deviceId: device))
        #expect(try EnrollmentStore(secure: secure).record(identity).epochAnchor == durable.epochAnchor)
        await restarted.close()
    }

    @Test func failedAnchorReplacementClearsAnAlreadyUnlockedClient() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let secure = InMemorySecureStore()
        let device = UUID().uuidString.lowercased()
        let native = session(server, store: secure, deviceId: device, directory: temporaryDirectory())
        _ = try await native.importCredential(server.credential(deviceId: device))
        _ = try await native.unlock(passphrase: passphrase)
        // Install one anchor, then attempt a conflicting fingerprint at the same floor.
        _ = try await native.importCredential(server.credential(deviceId: device, anchor: (1, String(repeating: "aa", count: 32))))
        await #expect(throws: ClientError.core(.InvalidProfile)) {
            try await native.importCredential(server.credential(deviceId: device, anchor: (1, String(repeating: "bb", count: 32))))
        }
        await #expect(throws: ClientError.notEnrolled) { try await native.conversations() }
        #expect(try await native.open().keysUnlocked == false)
        await #expect(throws: ClientError.core(.InvalidProfile)) { try await native.unlock(passphrase: passphrase) }
        await native.close()
    }

    @Test(arguments: [1, 2])
    func interruptedFirstImportCannotReplaceItsPendingPin(incomingFloor: Int) async throws {
        let server = try FakeServer(passphrase: passphrase)
        let secure = RecoverySecureStore()
        let device = UUID().uuidString.lowercased()
        let identity = try DeviceCredential.parse(server.credential(deviceId: device), allowLoopbackHTTP: false).identity
        var vault = server.vault(deviceId: device)
        vault["key_epoch"] = 2
        let response = try JSONSerialization.data(withJSONObject: vault)
        server.mutate { $0.override = { request in
            request.url.path == "/v1/vault" ? HTTPResponse(status: 200, url: request.url, body: response) : nil
        } }
        let native = session(server, store: secure, deviceId: device, directory: temporaryDirectory())
        secure.failingRead = identity.account("database-key")
        await #expect(throws: recoveryStoreFailure) {
            try await native.importCredential(server.credential(deviceId: device, anchor: (2, String(repeating: "aa", count: 32))))
        }
        let original = try #require(secure.value(identity.account("enrollment")))
        // Core has never opened: the secure record alone must enforce monotonicity.
        secure.failingRead = nil
        await #expect(throws: ClientError.core(.InvalidProfile)) {
            try await native.importCredential(server.credential(deviceId: device, anchor: (incomingFloor, String(repeating: "bb", count: 32))))
        }
        #expect(secure.value(identity.account("enrollment")) == original)
        #expect(try EnrollmentStore(secure: secure).record(identity).epochFloor == 2)
        #expect(secure.value(identity.account("database-created")) == nil)
    }

    @Test func durableEnrollmentAnchorRejectsPartialMalformedAndNullFields() throws {
        let identity = EnrollmentIdentity(origin: FakeServer.origin, vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased())
        let record = EnrollmentRecord(identity: identity, role: "device", keyEpoch: 1, profileJson: "{}", headerJson: "{}")
        let secure = InMemorySecureStore()
        let store = EnrollmentStore(secure: secure)
        try store.saveRecord(record)
        #expect(try store.record(identity).epochAnchor == nil, "legacy records omit both optional fields")
        let legacy = String(decoding: try JSONEncoder().encode(record), as: UTF8.self)
        for fields in [
            #""epochFloor":1"#,
            #""profileFingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#,
            #""epochFloor":null,"profileFingerprint":null"#,
            #""epochFloor":0,"profileFingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#,
            #""epochFloor":1.0,"profileFingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#,
            #""epochFloor":true,"profileFingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#,
            #""epochFloor":1,"profileFingerprint":"AA""#,
        ] {
            let malformed = String(legacy.dropLast()) + "," + fields + "}"
            try secure.upsert(identity.account("enrollment"), Data(malformed.utf8))
            #expect(throws: ClientError.missingSecret("enrollment record")) { try store.record(identity) }
        }
    }

    @Test func importVerifiesIdentityUnlocksWithSharedPassphraseAndRestoresKeysWithoutIt() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let directory = temporaryDirectory()
        let first = session(server, store: store, deviceId: deviceId, directory: directory)

        let imported = try await first.importCredential(server.credential(deviceId: deviceId))
        #expect(server.paths == ["GET /v1/vault"])
        #expect(server.requests.first?.headers["Authorization"] == "Bearer \(server.token)")
        #expect(imported.databaseOpen)
        #expect(!imported.keysUnlocked, "an open database does not mean the vault keys are unlocked")
        let identity = try #require(imported.identity)
        let databaseKey = try #require(store.value(identity.account("database-key")))
        #expect(databaseKey.count == 32)
        #expect(store.value(identity.account("device-token")) == Data(server.token.utf8))

        await #expect(throws: ClientError.core(.WrongPassphrase)) { try await first.unlock(passphrase: "wrong passphrase") }
        let unlocked = try await first.unlock(passphrase: passphrase)
        #expect(unlocked.keysUnlocked)
        #expect(store.value(identity.account("key-cache-1")) != nil)
        #expect(store.value(identity.account("vault-secret-v2")) == Data(passphrase.utf8))

        // Re-import of the same identity refreshes metadata but never replaces the database key.
        _ = try await first.importCredential(server.credential(deviceId: deviceId))
        #expect(store.value(identity.account("database-key")) == databaseKey)
        await first.close()

        // A fresh process: the Keychain key cache restores the keys without the passphrase.
        let second = session(server, store: store, deviceId: deviceId, directory: directory)
        let reopened = try await second.open()
        #expect(reopened.databaseOpen && reopened.keysUnlocked && reopened.rejectedKeyCaches == 0)
        await second.close()
    }

    @Test func nothingIsStoredWhenTheServerAnswersForAnotherDevice() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let session = session(server, store: store, deviceId: UUID().uuidString.lowercased(), directory: temporaryDirectory())
        await #expect(throws: ClientError.identityMismatch) {
            try await session.importCredential(server.credential(deviceId: UUID().uuidString.lowercased()))
        }
        #expect(store.accounts.isEmpty)
    }

    @Test func computerApprovalFactoryRequiresUnlockedOwnerKeys() async throws {
        let server = try FakeServer(passphrase: passphrase)
        server.mutate { $0.role = "owner" }
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let session = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        _ = try await session.importCredential(server.credential(deviceId: deviceId))
        await #expect(throws: ClientError.notEnrolled) {
            try await session.ownerComputerApprovalHost(deviceName: "iPhone")
        }
        _ = try await session.unlock(passphrase: passphrase)
        _ = try await session.ownerComputerApprovalHost(deviceName: "iPhone")
        await session.close()
    }

    @Test func nothingIsStoredWhenTheTokenIsRejected() async throws {
        let server = try FakeServer(passphrase: passphrase)
        server.mutate { $0.override = { request in HTTPResponse(status: 401, url: request.url, body: Data("{}".utf8)) } }
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let session = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        await #expect(throws: ClientError.unauthorized) { try await session.importCredential(server.credential(deviceId: deviceId)) }
        #expect(store.accounts.isEmpty)
    }

    @Test func vaultBindingCleanupRetriesLocallyAfterConfirmedVaultDeletion() async throws {
        let server = try FakeServer(passphrase: passphrase)
        server.mutate { server in
            server.override = { request in
                request.method == "DELETE" && request.url.path == "/v1/vault"
                    ? HTTPResponse(status: 204, url: request.url, body: Data())
                    : nil
            }
        }
        let store = BindingDeleteFaultStore()
        let deviceId = UUID().uuidString.lowercased()
        let nativeSession = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        let imported = try await nativeSession.importCredential(server.credential(deviceId: deviceId))
        let identity = try #require(imported.identity)
        _ = try await nativeSession.unlock(passphrase: passphrase)
        try store.upsert("vault-secret-pending-v2:\(identity.origin)", Data("pending recovery secret".utf8))
        store.failingDeleteAccount = identity.account("vault-secret-v2")

        await #expect(throws: ClientError.secureStorage(status: -1)) {
            try await nativeSession.deleteVault(vaultId: identity.vaultId)
        }
        #expect(store.value(EnrollmentStore.activeAccount) == nil)
        #expect(store.value(identity.account("vault-secret-v2")) != nil)
        #expect(store.value("vault-secret-pending-v2:\(identity.origin)") == Data("pending recovery secret".utf8))

        store.failingDeleteAccount = nil
        let restored = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        let status = try await restored.open()
        #expect(status.identity == nil)
        #expect(store.value(identity.account("vault-secret-v2")) == nil)
        #expect(server.paths.filter { $0 == "DELETE /v1/vault" }.count == 1)
    }

    @Test func failedKeyCacheEnumerationPublishesNoCandidateAndSecondOpenRestoresWithoutPassphrase() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = RecoverySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let directory = temporaryDirectory()
        let first = session(server, store: store, deviceId: deviceId, directory: directory)
        let identity = try #require(try await first.importCredential(server.credential(deviceId: deviceId)).identity)
        _ = try await first.unlock(passphrase: passphrase)
        await first.close()
        let cacheAccount = identity.account("key-cache-1")
        let cache = try #require(store.value(cacheAccount))
        let enrollment = try #require(store.value(identity.account("enrollment")))
        let databaseKey = try #require(store.value(identity.account("database-key")))
        let requestsBeforeOpen = server.paths

        let second = session(server, store: store, deviceId: deviceId, directory: directory)
        store.failingRead = cacheAccount
        await #expect(throws: recoveryStoreFailure) { try await second.open() }
        await #expect(throws: ClientError.notEnrolled) { try await second.conversations() }
        store.failingRead = nil

        let reopened = try await second.open()
        #expect(reopened.databaseOpen && reopened.keysUnlocked && reopened.rejectedKeyCaches == 0)
        _ = try await second.conversations()
        #expect(store.value(cacheAccount) == cache)
        #expect(store.value(identity.account("enrollment")) == enrollment)
        #expect(store.value(identity.account("database-key")) == databaseKey)
        #expect(server.paths == requestsBeforeOpen, "restoring the cached keys must not touch the server")
        await second.close()
    }

    @Test func confirmedVaultDeleteRetirementFailureKeepsMarkerUntilFreshRetryDrainsOldRoute() async throws {
        let server = try FakeServer(passphrase: passphrase)
        server.mutate { server in
            server.override = { request in
                request.method == "DELETE" && request.url.path == "/v1/vault"
                    ? HTTPResponse(status: 204, url: request.url, body: Data())
                    : nil
            }
        }
        let store = RecoverySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let transport = RelayCleanupTransport(inner: DeviceScopedTransport(server: server, deviceId: deviceId))
        let native = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        let identity = try #require(try await native.importCredential(server.credential(deviceId: deviceId)).identity)
        _ = try await native.unlock(passphrase: passphrase)

        let relayOrigin = "https://relay.example.test"
        let wakeIdentity = RelayWakeIdentity(origin: identity.origin, vaultId: identity.vaultId, deviceId: identity.deviceId, environment: "production")
        try await native.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        let route = RelayWakeRoute(
            routeId: UUID().uuidString.lowercased(), manageCredential: String(repeating: "a", count: 64),
            wakeCredential: String(repeating: "b", count: 64), tokenFingerprint: "token"
        )
        let routeAccount = "relay-wake-v2|\(identity.origin)|\(identity.vaultId)|\(identity.deviceId)|production|\(relayOrigin)|route"
        try store.upsert(routeAccount, JSONEncoder().encode(route))
        let routeBytes = try #require(store.value(routeAccount))
        let mine = RelayWakeCleanupContext(identity: wakeIdentity, relayOrigin: relayOrigin, deviceCredential: server.token, retiring: false)
        let unrelated = RelayWakeCleanupContext(
            identity: RelayWakeIdentity(origin: identity.origin, vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production"),
            relayOrigin: relayOrigin, deviceCredential: "unrelated-bearer", retiring: false
        )
        try store.upsert(relayContextsAccount, JSONEncoder().encode(try storedRelayContexts(store) + [unrelated]))

        // Only the protected relay-cleanup-context write fails; the marker, binding and pointer writes succeed.
        store.failingWrite = relayContextsAccount
        await #expect(throws: recoveryStoreFailure) { try await native.deleteVault(vaultId: identity.vaultId) }
        store.failingWrite = nil

        #expect(store.value(EnrollmentStore.activeAccount) == nil, "local teardown deactivates the enrollment pointer")
        #expect(store.value(identity.account("vault-secret-v2")) == nil, "local teardown removes the device binding")
        #expect(storedMarker(store) == identity, "the durable marker survives until binding and contexts resolve")
        #expect(store.value(routeAccount) == routeBytes, "captured route material is retained for the retry")
        #expect(try storedRelayContexts(store) == [mine, unrelated])
        #expect(server.paths.filter { $0 == "DELETE /v1/vault" }.count == 1)
        #expect(transport.cleanupRequests.isEmpty)

        let fresh = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await fresh.deleteVault(vaultId: identity.vaultId)
        let retiring = RelayWakeCleanupContext(identity: wakeIdentity, relayOrigin: relayOrigin, deviceCredential: server.token, retiring: true)
        #expect(try await fresh.retiringRelayWakeContexts() == [retiring], "the retry retires only the deleted identity's context")
        #expect(storedMarker(store) == identity, "retiring a context is not resolution: the marker stays")
        #expect(transport.cleanupRequests.isEmpty)

        await fresh.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        let cleanup = transport.cleanupRequests
        #expect(cleanup.map { "\($0.method) \($0.url.path)" } == [
            "DELETE /v1/devices/self/wake-route", "POST /v1/routes/\(route.routeId)/revoke",
        ])
        #expect(cleanup.first?.headers["Authorization"] == "Bearer \(server.token)", "server removal uses the captured credential")
        let revokeBody = try JSONSerialization.jsonObject(with: try #require(cleanup.last?.body)) as? [String: Any]
        #expect(revokeBody?["manage_credential"] as? String == route.manageCredential)
        #expect(try await fresh.retiringRelayWakeContexts().isEmpty)
        #expect(store.value(routeAccount) == nil, "the matching old route is revoked and removed")
        #expect(storedMarker(store) == nil, "the marker clears only after binding, contexts and route cleanup resolve")
        #expect(try storedRelayContexts(store) == [unrelated], "the live unrelated identity is never retired")
        #expect(!cleanup.contains { $0.headers["Authorization"] == "Bearer unrelated-bearer" })
    }

    @Test func aSecondEnrollmentIsRefusedAndTheFirstIsKept() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let first = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        _ = try await first.importCredential(server.credential(deviceId: deviceId))
        let before = store.accounts

        let otherDevice = UUID().uuidString.lowercased()
        let other = session(server, store: store, deviceId: otherDevice, directory: temporaryDirectory())
        await #expect(throws: ClientError.alreadyEnrolled) { try await other.importCredential(server.credential(deviceId: otherDevice)) }
        #expect(store.accounts == before)
        await first.close()
    }

    @Test func foregroundSyncThroughTheSessionUsesTheStoredToken() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let session = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        _ = try await session.importCredential(server.credential(deviceId: deviceId))
        _ = try await session.unlock(passphrase: passphrase)
        let before = server.paths.count
        let report = try await session.syncOnce()
        #expect(report.complete && report.uploaded == 0 && report.receiveCursor == "0")
        let paths = Array(server.paths.dropFirst(before))
        let replay = try #require(paths.firstIndex(of: "GET /v1/events"))
        let capabilities = paths.indices.filter { paths[$0] == "PUT /v1/devices/self/retention-capability" }
        #expect(capabilities.count == 2)
        #expect(capabilities[0] < replay && replay < capabilities[1])
        await session.close()
        await #expect(throws: ClientError.notEnrolled) { try await session.syncOnce() }
    }

    @Test func snapshotTooLargeMarkerSkipsUntilTimeOrBuildChangesAndClearsAfterBegin() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        _ = try producer.captureIncoming(sms: NativeIncomingSms(
            conversationId: nil, senderAddress: "+15550001111", body: "history",
            providerMessageId: UUID().uuidString, imported: false
        ))
        _ = try await ForegroundSync(client: producer, server: serverClient(server), vaultId: server.vaultId).run()
        server.mutate { server in
            server.replayFloor = 1
            server.override = { request in
                guard request.url.path == "/v1/snapshot" else { return nil }
                return server.reply(request, 200, [
                    "snapshot_version": 1, "compaction_supported": true, "compaction_active": true, "compaction_generation": "0",
                    "vault_id": server.vaultId, "high_water_cursor": "1000001", "record_count": "1000001",
                    "replay_floor_cursor": "1", "key_epoch": 1, "profile_fingerprint": "unused", "max_page_size": 200,
                ])
            }
        }
        let suite = "peppy-snapshot-marker-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let markerStore = SnapshotTooLargeStore(defaults: defaults)
        let now = Locked(Date(timeIntervalSinceReferenceDate: 0))
        let deviceID = UUID().uuidString.lowercased()
        let makeSession: (String) -> NativeSession = { build in
            NativeSession(
                secureStore: InMemorySecureStore(), transport: DeviceScopedTransport(server: server, deviceId: deviceID),
                databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false, snapshotTooLargeStore: markerStore,
                appBuild: build, snapshotClock: { now.value }
            )
        }
        let first = makeSession("1")
        _ = try await first.importCredential(server.credential(deviceId: deviceID))
        _ = try await first.unlock(passphrase: passphrase)
        let rejected = try await first.syncOnce()
        #expect(rejected.historyTooLarge)
        #expect(markerStore.marker(for: server.vaultId)?.build == "1")

        let beforeSkip = server.paths.count
        let skipped = try await first.syncOnce()
        #expect(skipped.historyTooLarge)
        #expect(
            Array(server.paths.dropFirst(beforeSkip)).filter { $0 == "GET /v1/events" }.count == 1,
            "a gated begin ends the pass; the replay 409 is not retried"
        )
        #expect(Array(server.paths.dropFirst(beforeSkip)).filter { $0 == "GET /v1/snapshot" }.count == 1, "the only cut read is the capability probe")

        now.withValue { $0 = $0.addingTimeInterval(6 * 60 * 60) }
        let beforeRetry = server.paths.count
        let retry = try await first.syncOnce()
        #expect(retry.historyTooLarge)
        #expect(Array(server.paths.dropFirst(beforeRetry)).filter { $0 == "GET /v1/snapshot" }.count == 2)

        server.mutate { $0.override = nil }
        let upgraded = makeSession("2")
        _ = try await upgraded.importCredential(server.credential(deviceId: deviceID))
        _ = try await upgraded.unlock(passphrase: passphrase)
        let success = try await upgraded.syncOnce()
        #expect(!success.historyTooLarge)
        #expect(markerStore.marker(for: server.vaultId) == nil)

        markerStore.mark(vaultID: server.vaultId, build: "2", now: now.value)
        _ = try await upgraded.disconnectLocalOnly()
        #expect(markerStore.marker(for: server.vaultId) == nil, "leaving the enrollment clears its too-large marker")
        try producer.dispose()
    }

    /// Self-revoke and vault deletion both leave the enrollment through `changeEnrollment`, which clears its marker.
    @Test func selfRevokeAndVaultDeletionClearTheTooLargeMarker() async throws {
        for leave in ["revoke", "delete"] {
            let server = try FakeServer(passphrase: passphrase)
            let device = UUID().uuidString.lowercased()
            server.mutate { server in
                server.override = { request in
                    if leave == "revoke", request.url.path.hasSuffix("/revoke") { return server.reply(request, 200, [:]) }
                    if leave == "delete", request.method == "DELETE", request.url.path == "/v1/vault" {
                        return server.reply(request, 200, [:])
                    }
                    return nil
                }
            }
            let suite = "peppy-marker-leave-\(UUID().uuidString)"
            let defaults = try #require(UserDefaults(suiteName: suite))
            defer { defaults.removePersistentDomain(forName: suite) }
            let markerStore = SnapshotTooLargeStore(defaults: defaults)
            let session = NativeSession(
                secureStore: InMemorySecureStore(), transport: DeviceScopedTransport(server: server, deviceId: device),
                databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false, snapshotTooLargeStore: markerStore
            )
            _ = try await session.importCredential(server.credential(deviceId: device))
            _ = try await session.unlock(passphrase: passphrase)
            markerStore.mark(vaultID: server.vaultId, build: "1", rejection: .append)

            if leave == "revoke" {
                _ = try await session.revokeDevice(device)
            } else {
                _ = try await session.deleteVault(vaultId: server.vaultId)
            }
            #expect(markerStore.marker(for: server.vaultId) == nil, "leaving by \(leave) clears the marker")
        }
    }

    @Test func delayedOldReadinessResponseCannotMarkSameIdentityReopenReady() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let gate = ReadinessResponseGate()
        let transport = DelayedReadinessTransport(
            inner: DeviceScopedTransport(server: server, deviceId: deviceId), gate: gate
        )
        let native = NativeSession(
            secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )
        _ = try await native.importCredential(server.credential(deviceId: deviceId))
        _ = try await native.unlock(passphrase: passphrase)
        let oldPass = Task { try await native.syncOnce() }
        await gate.waitUntilDelayed()

        await native.close()
        _ = try await native.open()
        await gate.resume()
        await #expect(throws: ClientError.canceled) { try await oldPass.value }
        _ = try await native.syncOnce()

        let readiness = server.lock.withLock { server.requests.compactMap(Self.cleanupReady) }
        #expect(readiness.suffix(4) == [false, true, false, true])
        await native.close()
    }

    @Test func wrongUnlockDoesNotBindAndVerifiedUnlockNeverOverwrites() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let store = InMemorySecureStore()
        let deviceId = UUID().uuidString.lowercased()
        let native = session(server, store: store, deviceId: deviceId, directory: temporaryDirectory())
        let imported = try await native.importCredential(server.credential(deviceId: deviceId))
        let identity = try #require(imported.identity)
        let binding = identity.account("vault-secret-v2")

        await #expect(throws: ClientError.core(.WrongPassphrase)) {
            try await native.unlock(passphrase: "wrong passphrase")
        }
        #expect(store.value(binding) == nil)

        _ = try await native.unlock(passphrase: passphrase)
        #expect(store.value(binding) == Data(passphrase.utf8))
        try store.upsert(binding, Data("existing conflicting secret".utf8))
        _ = try await native.unlock(passphrase: passphrase)
        #expect(store.value(binding) == Data("existing conflicting secret".utf8))
    }

    @Test func verifiedUnlockSurvivesBindingStorageFailure() async throws {
        let readFailure = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await readFailure.importIdentity()
        readFailure.store.failingRead = readFailure.bindingAccount
        let readUnlocked = try await readFailure.session.unlock(passphrase: passphrase)
        #expect(readUnlocked.keysUnlocked)
        #expect(readFailure.store.value(readFailure.bindingAccount) == nil)

        let writeFailure = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await writeFailure.importIdentity()
        writeFailure.store.failingWrite = writeFailure.bindingAccount
        let writeUnlocked = try await writeFailure.session.unlock(passphrase: passphrase)
        #expect(writeUnlocked.keysUnlocked)
        #expect(writeFailure.store.value(writeFailure.bindingAccount) == nil)
    }

    @Test func offlineRevealRepairsBindingWithoutUnlockingCore() async throws {
        let fixture = try RecoverySessionFixture(passphrase: passphrase)
        let imported = try await fixture.session.importCredential(
            fixture.server.credential(deviceId: fixture.identity.deviceId, origin: fixture.identity.origin)
        )
        #expect(!imported.keysUnlocked)
        #expect(fixture.store.value(fixture.bindingAccount) == nil)
        #expect(fixture.store.value(fixture.checkpointAccount) == fixture.checkpoint)
        let sessionAccount = "hosted-account-session:\(fixture.identity.origin)"
        #expect(fixture.store.value(sessionAccount) == nil)
        let expired = ExpiredHostedSession(
            accountID: UUID().uuidString.lowercased(),
            bearer: "pst_" + String(repeating: "a", count: 64),
            expiresAt: .distantPast
        )
        try fixture.store.upsert(sessionAccount, try JSONEncoder().encode(expired))
        let requestsBeforeReveal = fixture.server.paths

        let revealed = try await fixture.session.ownerVaultSecret()

        #expect(revealed == passphrase)
        #expect(fixture.store.value(fixture.bindingAccount) == Data(passphrase.utf8))
        #expect(fixture.store.value(fixture.pendingAccount) == nil)
        #expect(fixture.store.value(fixture.checkpointAccount) == nil)
        let status = try await fixture.session.open()
        #expect(!status.keysUnlocked, "pure header verification must not call NativeClient.unlock")
        #expect(fixture.store.value(fixture.identity.account("key-cache-1")) == nil)
        #expect(fixture.server.paths == requestsBeforeReveal, "offline repair must not use a hosted or device session")
    }

    @Test func offlineRevealPreservesWrongPhraseAndWrongTupleRecovery() async throws {
        let wrongPhrase = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await wrongPhrase.importIdentity()
        try wrongPhrase.store.upsert(wrongPhrase.pendingAccount, Data("wrong phrase with enough words here".utf8))
        await #expect(throws: ClientError.core(.WrongPassphrase)) {
            try await wrongPhrase.session.ownerVaultSecret()
        }
        #expect(wrongPhrase.store.value(wrongPhrase.bindingAccount) == nil)
        #expect(wrongPhrase.store.value(wrongPhrase.pendingAccount) != nil)
        #expect(wrongPhrase.store.value(wrongPhrase.checkpointAccount) == wrongPhrase.checkpoint)

        let wrongTuple = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await wrongTuple.importIdentity()
        let other = try prepareHostedProvisioning(
            origin: wrongTuple.identity.origin,
            accountId: UUID().uuidString.lowercased(),
            passphrase: passphrase,
            clientKind: .ios,
            deviceName: "Other recovery"
        )
        let otherCheckpoint = try other.checkpoint()
        try wrongTuple.store.upsert(wrongTuple.checkpointAccount, otherCheckpoint)
        await #expect(throws: (any Error).self) { try await wrongTuple.session.ownerVaultSecret() }
        #expect(wrongTuple.store.value(wrongTuple.bindingAccount) == nil)
        #expect(wrongTuple.store.value(wrongTuple.pendingAccount) == Data(passphrase.utf8))
        #expect(wrongTuple.store.value(wrongTuple.checkpointAccount) == otherCheckpoint)
    }

    @Test func offlineRevealRetriesAfterPartialCleanup() async throws {
        let fixture = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await fixture.importIdentity()
        fixture.store.failingDelete = fixture.pendingAccount

        await #expect(throws: recoveryStoreFailure) { try await fixture.session.ownerVaultSecret() }
        #expect(fixture.store.value(fixture.bindingAccount) == Data(passphrase.utf8))
        #expect(fixture.store.value(fixture.pendingAccount) == Data(passphrase.utf8))
        #expect(fixture.store.value(fixture.checkpointAccount) == fixture.checkpoint)

        fixture.store.failingDelete = fixture.checkpointAccount
        #expect(try await fixture.session.ownerVaultSecret() == passphrase)
        #expect(fixture.store.value(fixture.pendingAccount) == nil)
        #expect(fixture.store.value(fixture.checkpointAccount) == fixture.checkpoint)

        fixture.store.failingDelete = nil
        #expect(try await fixture.session.ownerVaultSecret() == passphrase)
        #expect(fixture.store.value(fixture.checkpointAccount) == nil)
    }

    @Test func offlineRevealNeverDeletesAChangedCheckpoint() async throws {
        let fixture = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await fixture.importIdentity()
        let replacement = Data("newer checkpoint bytes".utf8)
        fixture.store.replaceOnRead = (fixture.checkpointAccount, 3, replacement)

        await #expect(throws: ClientError.identityMismatch) { try await fixture.session.ownerVaultSecret() }
        #expect(fixture.store.value(fixture.bindingAccount) == nil)
        #expect(fixture.store.value(fixture.pendingAccount) == Data(passphrase.utf8))
        #expect(fixture.store.value(fixture.checkpointAccount) == replacement)
    }

    @Test func offlineRevealPreservesConflictAndReadFailure() async throws {
        let conflict = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await conflict.importIdentity()
        let existing = "existing verified binding"
        try conflict.store.upsert(conflict.bindingAccount, Data(existing.utf8))
        #expect(try await conflict.session.ownerVaultSecret() == existing)
        #expect(conflict.store.value(conflict.bindingAccount) == Data(existing.utf8))
        #expect(conflict.store.value(conflict.pendingAccount) == Data(passphrase.utf8))
        #expect(conflict.store.value(conflict.checkpointAccount) == conflict.checkpoint)

        let unreadable = try RecoverySessionFixture(passphrase: passphrase)
        _ = try await unreadable.importIdentity()
        unreadable.store.failingRead = unreadable.checkpointAccount
        await #expect(throws: recoveryStoreFailure) { try await unreadable.session.ownerVaultSecret() }
        unreadable.store.failingRead = nil
        #expect(unreadable.store.value(unreadable.bindingAccount) == nil)
        #expect(unreadable.store.value(unreadable.pendingAccount) == Data(passphrase.utf8))
        #expect(unreadable.store.value(unreadable.checkpointAccount) == unreadable.checkpoint)
    }

    @Test func storageStatusRejectsMalformedUTF8AndOversizedBodies() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let deviceId = UUID().uuidString.lowercased()
        let scoped = session(server, store: InMemorySecureStore(), deviceId: deviceId, directory: temporaryDirectory())
        _ = try await scoped.importCredential(server.credential(deviceId: deviceId))

        server.mutate { fake in
            fake.override = { request in
                request.url.path == "/v1/attachments/storage/status"
                    ? HTTPResponse(status: 200, url: request.url, body: Data([0xff])) : nil
            }
        }
        await #expect(throws: ClientError.invalidResponse("retention status")) {
            _ = try await scoped.attachmentStorageStatus()
        }

        server.mutate { fake in
            fake.override = { request in
                request.url.path == "/v1/attachments/storage/status"
                    ? HTTPResponse(
                        status: 200, url: request.url,
                        body: Data(repeating: 0x20, count: Int(attachmentRetentionResponseReadLimitBytes()) + 1)
                    ) : nil
            }
        }
        await #expect(throws: (any Error).self) { _ = try await scoped.attachmentStorageStatus() }
        await scoped.close()
    }

}

private struct RecoverySessionFixture {
    let store = RecoverySecureStore()
    let server: FakeServer
    let session: NativeSession
    let identity: EnrollmentIdentity
    let checkpoint: Data
    let checkpointAccount: String
    let pendingAccount: String
    let bindingAccount: String

    func importIdentity() async throws -> SessionStatus {
        try await session.importCredential(server.credential(deviceId: identity.deviceId, origin: identity.origin))
    }

    init(passphrase: String) throws {
        let origin = FakeServer.origin
        let accountID = UUID().uuidString.lowercased()
        let provisioning = try prepareHostedProvisioning(
            origin: origin,
            accountId: accountID,
            passphrase: passphrase,
            clientKind: .ios,
            deviceName: "Recovery test"
        )
        let view = try provisioning.view()
        identity = EnrollmentIdentity(origin: origin, vaultId: view.vaultId, deviceId: view.deviceId)
        checkpoint = try provisioning.checkpoint()
        checkpointAccount = "hosted-provisioning:\(origin)"
        pendingAccount = "vault-secret-pending-v2:\(origin)|hosted-account|\(accountID)"
        bindingAccount = identity.account("vault-secret-v2")
        try store.upsert(checkpointAccount, checkpoint)
        try store.upsert(pendingAccount, Data(passphrase.utf8))
        server = try FakeServer(vaultId: view.vaultId, passphrase: passphrase)
        session = NativeSession(
            secureStore: store,
            transport: DeviceScopedTransport(server: server, deviceId: view.deviceId),
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )
    }
}

private struct ExpiredHostedSession: Codable {
    let accountID: String
    let bearer: String
    let expiresAt: Date
}

private let recoveryStoreFailure = ClientError.secureStorage(status: -25308)

private final class RecoverySecureStore: SecureStore, @unchecked Sendable {
    private let backing = InMemorySecureStore()
    private let lock = NSLock()
    private var readCounts: [String: Int] = [:]
    var failingRead: String?
    var failingWrite: String?
    var failingDelete: String?
    var replaceOnRead: (account: String, count: Int, data: Data)?

    func value(_ account: String) -> Data? { backing.value(account) }

    func read(_ account: String) throws(ClientError) -> Data? {
        if failingRead == account { throw recoveryStoreFailure }
        let count = lock.withLock {
            readCounts[account, default: 0] += 1
            return readCounts[account]!
        }
        if let replacement = replaceOnRead,
           replacement.account == account, replacement.count == count {
            try backing.upsert(account, replacement.data)
        }
        return try backing.read(account)
    }

    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        if failingWrite == account { throw recoveryStoreFailure }
        return try backing.addIfAbsent(account, data)
    }

    func upsert(_ account: String, _ data: Data) throws(ClientError) {
        if failingWrite == account { throw recoveryStoreFailure }
        try backing.upsert(account, data)
    }

    func delete(_ account: String) throws(ClientError) {
        if failingDelete == account { throw recoveryStoreFailure }
        try backing.delete(account)
    }
}

private final class BindingDeleteFaultStore: SecureStore, @unchecked Sendable {
    private let backing = InMemorySecureStore()
    var failingDeleteAccount: String?

    func value(_ account: String) -> Data? { backing.value(account) }
    func read(_ account: String) throws(ClientError) -> Data? { try backing.read(account) }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        try backing.addIfAbsent(account, data)
    }
    func upsert(_ account: String, _ data: Data) throws(ClientError) { try backing.upsert(account, data) }
    func delete(_ account: String) throws(ClientError) {
        if account == failingDeleteAccount {
            throw .secureStorage(status: -1)
        }
        try backing.delete(account)
    }
}

private let relayContextsAccount = "relay-wake-cleanup-contexts-v1"
private let vaultBindingCleanupAccount = "vault-binding-cleanup-pending"

private func storedRelayContexts(_ store: RecoverySecureStore) throws -> [RelayWakeCleanupContext] {
    guard let data = store.value(relayContextsAccount) else { return [] }
    return try JSONDecoder().decode([RelayWakeCleanupContext].self, from: data)
}

private func storedMarker(_ store: RecoverySecureStore) -> EnrollmentIdentity? {
    store.value(vaultBindingCleanupAccount).flatMap { try? JSONDecoder().decode(EnrollmentIdentity.self, from: $0) }
}

/// Answers the two relay-cleanup calls (server route removal and relay revoke) itself, recording them,
/// and forwards every other request to the scoped vault server.
private final class RelayCleanupTransport: HTTPTransport, @unchecked Sendable {
    private let inner: any HTTPTransport
    private let lock = NSLock()
    private var cleanup: [HTTPRequest] = []

    init(inner: any HTTPTransport) {
        self.inner = inner
    }

    var cleanupRequests: [HTTPRequest] { lock.withLock { cleanup } }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let isRouteRemoval = request.method == "DELETE" && request.url.path == "/v1/devices/self/wake-route"
        let isRelayRevoke = request.method == "POST" && request.url.path.hasPrefix("/v1/routes/") && request.url.path.hasSuffix("/revoke")
        guard isRouteRemoval || isRelayRevoke else { return try await inner.send(request) }
        lock.withLock { cleanup.append(request) }
        return HTTPResponse(status: 204, url: request.url, body: Data())
    }
}

private actor ReadinessResponseGate {
    private var delayed = false
    private var delayWaiter: CheckedContinuation<Void, Never>?
    private var resumeWaiter: CheckedContinuation<Void, Never>?

    func delay() async {
        delayed = true
        delayWaiter?.resume()
        await withCheckedContinuation { resumeWaiter = $0 }
    }

    func waitUntilDelayed() async {
        guard !delayed else { return }
        await withCheckedContinuation { delayWaiter = $0 }
    }

    func resume() {
        resumeWaiter?.resume()
        resumeWaiter = nil
    }
}

private final class DelayedReadinessTransport: HTTPTransport, @unchecked Sendable {
    private let inner: any HTTPTransport
    private let gate: ReadinessResponseGate
    private let lock = NSLock()
    private var didDelay = false

    init(inner: any HTTPTransport, gate: ReadinessResponseGate) {
        self.inner = inner
        self.gate = gate
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        let response = try await inner.send(request)
        guard Self.cleanupReady(request) == true, lock.withLock({
            guard !didDelay else { return false }
            didDelay = true
            return true
        }) else { return response }
        await gate.delay()
        return response
    }

    private static func cleanupReady(_ request: HTTPRequest) -> Bool? {
        guard let body = request.body,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return nil }
        return object["cleanup_ready"] as? Bool
    }
}
