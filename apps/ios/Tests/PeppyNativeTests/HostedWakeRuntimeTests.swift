import Foundation
import XCTest
@testable import PeppyNative

final class HostedWakeRuntimeTests: XCTestCase {
    func testRetiringContextSurvivesNewSessionAndUsesCapturedBearer() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let context = RelayWakeCleanupContext(identity: identity, relayOrigin: "https://relay.example.test", deviceCredential: "old-bearer", retiring: true)
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([context]))
        let transport = CapabilityTransport(status: 204, body: [:])
        let recreated = NativeSession(secureStore: store, transport: transport,
                                      databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString), allowLoopbackHTTP: false)

        let restored = try await recreated.retiringRelayWakeContexts()
        XCTAssertEqual(restored, [context])
        let publisher = try await recreated.capturedRelayWakeRoutePublisher(context: context)
        try await publisher.remove(routeId: UUID().uuidString.lowercased())
        XCTAssertEqual(transport.request?.headers["Authorization"], "Bearer old-bearer")

        try await recreated.completeRetiringRelayWakeContext(context)
        let remaining = try await recreated.retiringRelayWakeContexts()
        XCTAssertTrue(remaining.isEmpty)
    }

    func testRecreatedPausedDrainRevokesStoredRouteBeforeRemovingContext() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let context = RelayWakeCleanupContext(identity: identity, relayOrigin: "https://relay.example.test", deviceCredential: "old-bearer", retiring: true)
        let route = RelayWakeRoute(routeId: UUID().uuidString.lowercased(), manageCredential: String(repeating: "a", count: 64), wakeCredential: String(repeating: "b", count: 64), tokenFingerprint: "token")
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([context]))
        let account = "relay-wake-v2|\(identity.origin)|\(identity.vaultId)|\(identity.deviceId)|\(identity.environment)|\(context.relayOrigin)|route"
        try store.upsert(account, JSONEncoder().encode(route))
        let transport = CleanupTransport()
        let recreated = NativeSession(secureStore: store, transport: transport,
                                      databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString), allowLoopbackHTTP: false)

        await recreated.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)

        XCTAssertEqual(transport.paths, ["DELETE /v1/devices/self/wake-route", "POST /v1/routes/\(route.routeId)/revoke"])
        XCTAssertEqual(transport.serverBearer, "Bearer old-bearer")
        let remaining = try await recreated.retiringRelayWakeContexts()
        XCTAssertTrue(remaining.isEmpty)
    }

    func testLegacyRouteMigratesToDurableCleanupBeforeSlotRemoval() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)
        // The v1 client keyed its slots by the relay origin. The server-origin slot is a decoy that
        // must not be migrated or removed by this relay client.
        let legacy = Self.legacyRoute()
        let decoy = Self.legacyRoute()
        let relayAccount = "relay-wake-v1|\(origin.serialized)|route"
        let decoyAccount = "relay-wake-v1|\(identity.origin)|route"
        try store.upsert(relayAccount, JSONSerialization.data(withJSONObject: legacy))
        try store.upsert(decoyAccount, JSONSerialization.data(withJSONObject: decoy))
        let client = RelayWakeClient(origin: origin, transport: CleanupTransport(), secure: store,
                                     publisher: NoopPublisher(), identity: identity)

        try await client.migrateLegacyRouteIfNeeded()
        XCTAssertNil(store.value(relayAccount))
        XCTAssertNotNil(store.value(decoyAccount))
        let hasCleanup = try await client.hasPendingCleanup()
        XCTAssertTrue(hasCleanup)

        // Durable: a fresh client over the same protected store still owns the queued cleanup.
        let recreated = RelayWakeClient(origin: origin, transport: CleanupTransport(), secure: store,
                                        publisher: NoopPublisher(), identity: identity)
        let stillQueued = try await recreated.hasPendingCleanup()
        XCTAssertTrue(stillQueued)
    }

    func testBoundPublisherTreatsOldServerDeleteAsTerminal() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let context = RelayWakeCleanupContext(identity: identity, relayOrigin: "https://relay.example.test", deviceCredential: "device-bearer", retiring: true)
        let transport = CapabilityTransport(status: 405, body: [:])
        let recreated = NativeSession(secureStore: store, transport: transport,
                                      databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)

        let publisher = try await recreated.capturedRelayWakeRoutePublisher(context: context)
        try await publisher.remove(routeId: UUID().uuidString.lowercased())
        XCTAssertEqual(transport.request?.method, "DELETE")
        XCTAssertEqual(transport.request?.headers["Authorization"], "Bearer device-bearer")
    }

    func testResumeReactivatesContextAfterSuccessfulPauseCleanup() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)

        try await session.retireRelayWakeContexts()
        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        XCTAssertTrue(try Self.contexts(store).isEmpty)

        // Resume in the same process reuses the bound client, so registration must run again.
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        let live = try Self.contexts(store)
        XCTAssertEqual(live.count, 1)
        XCTAssertEqual(live.first?.retiring, false)
    }

    func testOfflinePauseResumeNeverRevokesNewRouteThroughRetiringDuplicate() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        let route = RelayWakeRoute(routeId: UUID().uuidString.lowercased(), manageCredential: String(repeating: "c", count: 64), wakeCredential: String(repeating: "d", count: 64), tokenFingerprint: "token")
        let routeAccount = "relay-wake-v2|\(wakeIdentity.origin)|\(wakeIdentity.vaultId)|\(wakeIdentity.deviceId)|\(wakeIdentity.environment)|\(relayOrigin)|route"
        try store.upsert(routeAccount, JSONEncoder().encode(route))
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)

        // Offline pause: the context is retiring and its cleanup stays queued, never drained.
        try await session.retireRelayWakeContexts()
        XCTAssertEqual(try Self.contexts(store).first?.retiring, true)

        // Resume with the retiring duplicate still present must not drain the new live route.
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        XCTAssertTrue(transport.paths.isEmpty)
        let live = try Self.contexts(store)
        XCTAssertEqual(live.count, 1)
        XCTAssertEqual(live.first?.retiring, false)
    }

    func testRegistrationKeepsRetiringContextsOfOtherIdentities() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        let other = RelayWakeIdentity(origin: wakeIdentity.origin, vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: wakeIdentity.environment)
        let otherContext = RelayWakeCleanupContext(identity: other, relayOrigin: relayOrigin, deviceCredential: "other-bearer", retiring: true)
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([otherContext]))

        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)

        let remaining = try Self.contexts(store)
        XCTAssertTrue(remaining.contains(otherContext))
        XCTAssertEqual(remaining.filter { !$0.retiring }.count, 1)
    }

    func testCapabilityProbeUsesPinnedUnauthenticatedRequest() async throws {
        let transport = CapabilityTransport(status: 200, body: ["confirm_candidates": true])
        let session = NativeSession(
            secureStore: InMemorySecureStore(), transport: transport,
            databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),
            allowLoopbackHTTP: false
        )
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)

        let supportsCandidates = try await session.relayWakeSupportsCandidates(origin: origin)
        XCTAssertTrue(supportsCandidates)
        XCTAssertEqual(transport.request?.method, "GET")
        XCTAssertEqual(transport.request?.url.path, "/v1/capabilities")
        XCTAssertNil(transport.request?.headers["Authorization"])
    }

    func testMissingCapabilityUsesLegacyCompatibility() async throws {
        let transport = CapabilityTransport(status: 404, body: [:])
        let session = NativeSession(
            secureStore: InMemorySecureStore(), transport: transport,
            databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),
            allowLoopbackHTTP: false
        )
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)
        let supportsCandidates = try await session.relayWakeSupportsCandidates(origin: origin)
        XCTAssertFalse(supportsCandidates)
    }

    func testCapabilityProbeTreatsRateLimitAsRetryable() async throws {
        let transport = CapabilityTransport(status: 429, body: [:])
        let session = NativeSession(
            secureStore: InMemorySecureStore(), transport: transport,
            databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),
            allowLoopbackHTTP: false
        )
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)
        do {
            _ = try await session.relayWakeSupportsCandidates(origin: origin)
            XCTFail("Expected retryable server error")
        } catch let error as ClientError {
            guard case .server(429, _) = error else { return XCTFail("Unexpected error: \(error)") }
        }
    }
}

extension HostedWakeRuntimeTests {
    fileprivate static func legacyRoute() -> [String: String] {
        ["routeId": UUID().uuidString.lowercased(), "manageCredential": String(repeating: "a", count: 64), "wakeCredential": String(repeating: "b", count: 64)]
    }

    fileprivate static func contexts(_ store: InMemorySecureStore) throws -> [RelayWakeCleanupContext] {
        guard let data = store.value("relay-wake-cleanup-contexts-v1") else { return [] }
        return try JSONDecoder().decode([RelayWakeCleanupContext].self, from: data)
    }

    /// Seeds an active owner enrollment through the production store, then opens the session so
    /// registration sees the same loaded record a running app would.
    fileprivate static func enrolledSession(store: InMemorySecureStore, transport: any HTTPTransport) async throws -> (NativeSession, RelayWakeIdentity, String) {
        let identity = EnrollmentIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased())
        let record = EnrollmentRecord(identity: identity, role: "owner", keyEpoch: 1, profileJson: "{}", headerJson: "{}")
        try EnrollmentStore(secure: store).activate(record, token: "device-bearer")
        let session = NativeSession(secureStore: store, transport: transport,
                                    databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        try await session.open()
        let wakeIdentity = RelayWakeIdentity(origin: identity.origin, vaultId: identity.vaultId, deviceId: identity.deviceId, environment: "production")
        return (session, wakeIdentity, "https://relay.example.test")
    }
}

private final class CapabilityTransport: HTTPTransport, @unchecked Sendable {
    private let status: Int
    private let body: [String: Any]
    private let lock = NSLock()
    private var captured: HTTPRequest?

    var request: HTTPRequest? { lock.withLock { captured } }

    init(status: Int, body: [String: Any]) {
        self.status = status
        self.body = body
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { captured = request }
        return HTTPResponse(status: status, url: request.url, body: try JSONSerialization.data(withJSONObject: body))
    }
}

private final class CleanupTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var requests: [HTTPRequest] = []
    var paths: [String] { lock.withLock { requests.map { "\($0.method) \($0.url.path)" } } }
    var serverBearer: String? { lock.withLock { requests.first?.headers["Authorization"] } }
    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { requests.append(request) }
        return HTTPResponse(status: 204, url: request.url, body: Data())
    }
}

private struct NoopPublisher: RelayWakeRoutePublisher {
    func publish(routeId: String, wakeCredential: String) async throws {}
    func remove(routeId: String) async throws {}
}
