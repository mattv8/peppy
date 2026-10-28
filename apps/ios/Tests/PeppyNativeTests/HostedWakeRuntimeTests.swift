import Foundation
import Testing
@testable import PeppyNative

@Suite("Hosted wake runtime")
struct HostedWakeRuntimeTests {
    @Test
    func testRetiringContextSurvivesNewSessionAndUsesCapturedBearer() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let context = RelayWakeCleanupContext(identity: identity, relayOrigin: "https://relay.example.test", deviceCredential: "old-bearer", retiring: true)
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([context]))
        let transport = CapabilityTransport(status: 204, body: [:])
        let recreated = NativeSession(secureStore: store, transport: transport,
                                      databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString), allowLoopbackHTTP: false)

        let restored = try await recreated.retiringRelayWakeContexts()
        #expect(restored == [context])
        let publisher = try await recreated.capturedRelayWakeRoutePublisher(context: context)
        try await publisher.remove(routeId: UUID().uuidString.lowercased())
        #expect(transport.request?.headers["Authorization"] == "Bearer old-bearer")

        try await recreated.completeRetiringRelayWakeContext(context)
        let remaining = try await recreated.retiringRelayWakeContexts()
        #expect(remaining.isEmpty)
    }

    @Test
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

        #expect(transport.paths == ["DELETE /v1/devices/self/wake-route", "POST /v1/routes/\(route.routeId)/revoke"])
        #expect(transport.serverBearer == "Bearer old-bearer")
        let remaining = try await recreated.retiringRelayWakeContexts()
        #expect(remaining.isEmpty)
    }

    @Test
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
        #expect(store.value(relayAccount) == nil)
        #expect(store.value(decoyAccount) != nil)
        let hasCleanup = try await client.hasPendingCleanup()
        #expect(hasCleanup)

        // Durable: a fresh client over the same protected store still owns the queued cleanup.
        let recreated = RelayWakeClient(origin: origin, transport: CleanupTransport(), secure: store,
                                        publisher: NoopPublisher(), identity: identity)
        let stillQueued = try await recreated.hasPendingCleanup()
        #expect(stillQueued)
    }

    @Test
    func testBoundPublisherTreatsOldServerDeleteAsTerminal() async throws {
        let store = InMemorySecureStore()
        let identity = RelayWakeIdentity(origin: "https://server.example.test", vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: "production")
        let context = RelayWakeCleanupContext(identity: identity, relayOrigin: "https://relay.example.test", deviceCredential: "device-bearer", retiring: true)
        let transport = CapabilityTransport(status: 405, body: [:])
        let recreated = NativeSession(secureStore: store, transport: transport,
                                      databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)

        let publisher = try await recreated.capturedRelayWakeRoutePublisher(context: context)
        try await publisher.remove(routeId: UUID().uuidString.lowercased())
        #expect(transport.request?.method == "DELETE")
        #expect(transport.request?.headers["Authorization"] == "Bearer device-bearer")
    }

    @Test
    func testResumeReactivatesContextAfterSuccessfulPauseCleanup() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)

        try await session.retireRelayWakeContexts()
        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        #expect(try Self.contexts(store).isEmpty)

        // Resume in the same process reuses the bound client, so registration must run again.
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        let live = try Self.contexts(store)
        #expect(live.count == 1)
        #expect(live.first?.retiring == false)
    }

    @Test
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
        #expect(try Self.contexts(store).first?.retiring == true)

        // Resume with the retiring duplicate still present must not drain the new live route.
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        #expect(transport.paths.isEmpty)
        let live = try Self.contexts(store)
        #expect(live.count == 1)
        #expect(live.first?.retiring == false)
    }

    @Test
    func testRegistrationKeepsRetiringContextsOfOtherIdentities() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        let other = RelayWakeIdentity(origin: wakeIdentity.origin, vaultId: UUID().uuidString.lowercased(), deviceId: UUID().uuidString.lowercased(), environment: wakeIdentity.environment)
        let otherContext = RelayWakeCleanupContext(identity: other, relayOrigin: relayOrigin, deviceCredential: "other-bearer", retiring: true)
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([otherContext]))

        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)

        let remaining = try Self.contexts(store)
        #expect(remaining.contains(otherContext))
        #expect(remaining.filter { !$0.retiring }.count == 1)
    }

    @Test
    func testCapabilityProbeUsesPinnedUnauthenticatedRequest() async throws {
        let transport = CapabilityTransport(status: 200, body: ["confirm_candidates": true])
        let session = NativeSession(
            secureStore: InMemorySecureStore(), transport: transport,
            databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),
            allowLoopbackHTTP: false
        )
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)

        let supportsCandidates = try await session.relayWakeSupportsCandidates(origin: origin)
        #expect(supportsCandidates)
        #expect(transport.request?.method == "GET")
        #expect(transport.request?.url.path == "/v1/capabilities")
        #expect(transport.request?.headers["Authorization"] == nil)
    }

    @Test
    func testMissingCapabilityUsesLegacyCompatibility() async throws {
        let transport = CapabilityTransport(status: 404, body: [:])
        let session = NativeSession(
            secureStore: InMemorySecureStore(), transport: transport,
            databaseDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),
            allowLoopbackHTTP: false
        )
        let origin = try ServerOrigin(canonical: "https://relay.example.test", allowLoopbackHTTP: false)
        let supportsCandidates = try await session.relayWakeSupportsCandidates(origin: origin)
        #expect(!supportsCandidates)
    }

    @Test
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
            Issue.record("Expected retryable server error")
        } catch let error as ClientError {
            guard case .server(429, _) = error else {
                Issue.record("Unexpected error: \(error)")
                return
            }
        }
    }
}

extension HostedWakeRuntimeTests {
    @Test
    func testCoalescedWakeWorkRunsOverlappingSameKeyOnce() async {
        let coordinator = await MainActor.run { CoalescedWakeWork<Int>() }
        let probe = WakeWorkProbe()

        let first = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        await probe.waitForRuns(1)
        let second = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        try? await Task.sleep(for: .milliseconds(10))
        let runs = await probe.runCount
        #expect(runs == 1)

        await probe.releaseAll()
        await first.value
        await second.value
        let completed = await probe.completedCount
        #expect(completed == 1)
    }

    @Test
    func testCoalescedWakeWorkRunsDifferentKeysConcurrently() async {
        let coordinator = await MainActor.run { CoalescedWakeWork<Int>() }
        let probe = WakeWorkProbe()

        let first = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        await probe.waitForRuns(1)
        let second = Task { @MainActor in await coordinator.run(key: 2) { await probe.run() } }
        await probe.waitForRuns(2)
        let runs = await probe.runCount
        #expect(runs == 2)

        await probe.releaseAll()
        await first.value
        await second.value
        let completed = await probe.completedCount
        #expect(completed == 2)
    }

    @Test
    func testCoalescedWakeWorkCancelledWaiterReturnsWithoutCancellingWork() async {
        let coordinator = await MainActor.run { CoalescedWakeWork<Int>() }
        let probe = WakeWorkProbe()
        defer { Task { await probe.releaseAll() } }

        let first = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        await probe.waitForRuns(1)
        let joiner = Task { @MainActor in
            await coordinator.run(key: 1) { await probe.run() }
        }
        joiner.cancel()
        #expect(await finishes(joiner, within: .seconds(1)), "cancelled waiter returned")
        let runs = await probe.runCount
        #expect(runs == 1)

        await probe.releaseAll()
        await first.value
        await joiner.value
        let completed = await probe.completedCount
        #expect(completed == 1)

        let later = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        await probe.waitForRuns(2)
        await probe.releaseAll()
        await later.value
        let totalRuns = await probe.runCount
        #expect(totalRuns == 2)
    }

    @Test
    func testCoalescedWakeWorkCancelledWaiterDoesNotClearRunningWork() async {
        let coordinator = await MainActor.run { CoalescedWakeWork<Int>() }
        let probe = WakeWorkProbe()

        let first = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        await probe.waitForRuns(1)
        let cancelledJoiner = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        cancelledJoiner.cancel()
        await cancelledJoiner.value
        let laterJoiner = Task { @MainActor in await coordinator.run(key: 1) { await probe.run() } }
        try? await Task.sleep(for: .milliseconds(10))
        let runs = await probe.runCount
        #expect(runs == 1)

        await probe.releaseAll()
        await first.value
        await laterJoiner.value
        let completed = await probe.completedCount
        #expect(completed == 1)
    }

    @Test
    func testWakeFenceRetiresCapturedWorkWhenPauseWins() {
        var fence = WakeWorkFence()
        let captured = fence.generation
        #expect(fence.checkpoint(captured: captured, enabled: true) == .proceed)

        fence.preferenceChanged()
        #expect(fence.checkpoint(captured: captured, enabled: false) == .retirePaused)
    }

    @Test
    func testWakeFenceSupersedesCapturedWorkAfterPauseAndResume() {
        var fence = WakeWorkFence()
        let captured = fence.generation
        fence.preferenceChanged()
        fence.preferenceChanged()

        // A capability or registration answer from before the toggle must not continue on its own.
        #expect(fence.checkpoint(captured: captured, enabled: true) == .superseded)
    }

    @Test
    func testPauseWinningRegistrationRetiresAndDrainsTheLiveContext() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, relayOrigin) = try await Self.enrolledSession(store: store, transport: transport)
        var fence = WakeWorkFence()
        let captured = fence.generation

        // Registration completes after the pause began: the context is live until the checkpoint.
        try await session.registerRelayWakeContext(identity: wakeIdentity, relayOrigin: relayOrigin)
        fence.preferenceChanged()
        #expect(fence.checkpoint(captured: captured, enabled: false) == .retirePaused)

        // The sequence AppModel runs on .retirePaused: retire, then drain before returning.
        try await session.retireRelayWakeContexts()
        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)
        #expect(try Self.contexts(store).isEmpty)
    }

    @Test
    func testEnabledExitSweepDrainsRetiringContextOfAnotherRelay() async throws {
        let store = InMemorySecureStore()
        let transport = CleanupTransport()
        let (session, wakeIdentity, _) = try await Self.enrolledSession(store: store, transport: transport)
        // An unavailable enabled reconcile does not register; the sweep still drains older retiring contexts.
        let stale = RelayWakeCleanupContext(identity: wakeIdentity, relayOrigin: "https://old-relay.example.test", deviceCredential: "device-bearer", retiring: true)
        try store.upsert("relay-wake-cleanup-contexts-v1", JSONEncoder().encode([stale]))

        await session.drainRetiringRelayWakeContexts(relayTransport: transport, secure: store)

        #expect(try Self.contexts(store).isEmpty)
    }

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

private actor WakeWorkProbe {
    private var runs = 0
    private var completed = 0
    private var runWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    var runCount: Int { runs }
    var completedCount: Int { completed }

    func run() async {
        runs += 1
        resumeRunWaiters()
        await withCheckedContinuation { releaseWaiters.append($0) }
        completed += 1
    }

    func waitForRuns(_ count: Int) async {
        guard runs < count else { return }
        await withCheckedContinuation { runWaiters.append($0) }
        await waitForRuns(count)
    }

    func releaseAll() {
        let waiters = releaseWaiters
        releaseWaiters = []
        waiters.forEach { $0.resume() }
    }

    private func resumeRunWaiters() {
        let waiters = runWaiters
        runWaiters = []
        waiters.forEach { $0.resume() }
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

/// Waits for `task` at most `timeout` and reports whether it finished, so a regression fails the
/// expectation instead of hanging the suite. Whichever of completion or deadline comes first wins.
private func finishes(_ task: Task<Void, Never>, within timeout: Duration) async -> Bool {
    let pending = Locked<CheckedContinuation<Bool, Never>?>(nil)
    let settle: @Sendable (Bool) -> Void = { finished in
        let continuation = pending.withValue { stored in
            defer { stored = nil }
            return stored
        }
        continuation?.resume(returning: finished)
    }
    var deadline: Task<Void, Never>?
    let finished = await withCheckedContinuation { continuation in
        pending.withValue { $0 = continuation }
        Task { await task.value; settle(true) }
        deadline = Task { try? await Task.sleep(for: timeout); settle(false) }
    }
    deadline?.cancel()
    return finished
}
