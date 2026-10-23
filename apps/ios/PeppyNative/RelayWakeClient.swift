import CryptoKit
import Foundation
import Security

public struct RelayWakeRoute: Codable, Equatable, Sendable, CustomStringConvertible, CustomReflectable {
    public let routeId: String
    public let manageCredential: String
    public let wakeCredential: String
    public let tokenFingerprint: String

    public var description: String { "RelayWakeRoute(routeId: \(routeId), credentials: redacted)" }
    public var customMirror: Mirror { Mirror(self, children: ["routeId": routeId, "credentials": "<redacted>", "tokenFingerprint": "<redacted>"]) }
}

public struct RelayWakeIdentity: Codable, Equatable, Sendable {
    public let origin: String
    public let vaultId: String
    public let deviceId: String
    public let environment: String

    public init(origin: String, vaultId: String, deviceId: String, environment: String) {
        self.origin = origin
        self.vaultId = vaultId
        self.deviceId = deviceId
        self.environment = environment
    }
}

/// A relay selected for the current registration attempt. Community relays require an explicit,
/// canonical persisted opt-in; hosted eligibility and configuration remain Rust-owned.
public struct RelayWakeRegistrationTarget: Equatable, Sendable {
    public let origin: ServerOrigin
    public let environment: String

    public static func hosted(configuration: HostedWakeConfiguration) -> Self? {
        guard let origin = try? ServerOrigin(canonical: configuration.relayOrigin, allowLoopbackHTTP: false) else {
            return nil
        }
        return Self(origin: origin, environment: configuration.environment)
    }

    public static func custom(backgroundUpdatesEnabled: Bool, optedIn: Bool, persistedOrigin: String?, environment: String) -> Self? {
        guard backgroundUpdatesEnabled,
              optedIn,
              let persistedOrigin,
              let origin = try? ServerOrigin(canonical: persistedOrigin, allowLoopbackHTTP: false) else {
            return nil
        }
        return Self(origin: origin, environment: environment)
    }
}

public protocol RelayWakeRoutePublisher: Sendable {
    func publish(routeId: String, wakeCredential: String) async throws
    func remove(routeId: String) async throws
}

private struct PendingRegistration: Codable, Sendable {
    let registrationId: String
    let route: RelayWakeRoute
    let identity: RelayWakeIdentity
    let generation: UUID
    var challenge: String?
    var confirmed: Bool
    var createdAt: Date?
    var retry: RetryState?
}

private struct RetryState: Codable, Sendable {
    var attempts: UInt8
    var nextRetryAt: Date
}

private struct Cleanup: Codable, Sendable {
    let route: RelayWakeRoute
    var serverRemoved: Bool
    var relayRemoved: Bool
    var revision: UInt?
}

private struct LegacyRoute: Decodable {
    let routeId: String
    let manageCredential: String
    let wakeCredential: String
}

/// Durable, identity-scoped relay transport bookkeeping. The server and relay remain the
/// authorities; this actor only retains the exact proof and credentials needed to recover an
/// interrupted transport operation.
public actor RelayWakeClient {
    private let origin: ServerOrigin
    private let transport: any HTTPTransport
    private let secure: any SecureStore
    private let publisher: any RelayWakeRoutePublisher
    private let identity: RelayWakeIdentity
    private let supportsCandidates: Bool
    private let now: @Sendable () -> Date

    public enum RegistrationPhase: String, Sendable {
        case idle
        case pending
        case confirmedUnpublished
        case registered
        case paused
    }

    public struct RegistrationStatus: Sendable {
        public let phase: RegistrationPhase
        public let nextRetryAt: Date?
    }

    public init(
        origin: ServerOrigin,
        transport: any HTTPTransport,
        secure: any SecureStore,
        publisher: any RelayWakeRoutePublisher,
        identity: RelayWakeIdentity = RelayWakeIdentity(origin: "legacy", vaultId: "legacy", deviceId: "legacy", environment: "production"),
        supportsCandidates: Bool = true,
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.origin = origin
        self.transport = transport
        self.secure = secure
        self.publisher = publisher
        self.identity = identity
        self.supportsCandidates = supportsCandidates
        self.now = now
    }

    /// Moves the pre-v2 protected route into the durable cleanup queue before deleting it. The
    /// v1 client keyed its slots by the relay origin, not the server origin. The old record had no
    /// token fingerprint, which is irrelevant to conditional deletion.
    public func migrateLegacyRouteIfNeeded() throws {
        let legacyAccount = "relay-wake-v1|\(origin.serialized)|route"
        guard let data = try secure.read(legacyAccount) else { return }
        let legacy = try JSONDecoder().decode(LegacyRoute.self, from: data)
        guard Self.uuid(legacy.routeId) != nil, Self.secret(legacy.manageCredential), Self.secret(legacy.wakeCredential) else { return }
        try enqueue(RelayWakeRoute(routeId: legacy.routeId, manageCredential: legacy.manageCredential,
                                   wakeCredential: legacy.wakeCredential, tokenFingerprint: "legacy"))
        try secure.delete(legacyAccount)
    }

    /// Registers only once for a token. A matching pending candidate is deliberately retained:
    /// a later challenge or a retry must use the same proof, never allocate a new route.
    @discardableResult public func beginEnrollment(deviceToken: String) async throws -> Bool {
        guard !deviceToken.isEmpty, deviceToken.utf8.count <= 4096 else { return false }
        let fingerprint = Self.fingerprint(deviceToken)
        if let active = try route(), active.tokenFingerprint == fingerprint {
            return try await publishConfirmedRoute()
        }
        if let existing = try pending(), existing.route.tokenFingerprint == fingerprint {
            if isExpired(existing, now: now()) {
                try retire(existing)
                try secure.delete(account("pending"))
            } else {
            if let retry = existing.retry, retry.nextRetryAt > Date() { return false }
            if existing.confirmed { return try await publishConfirmed(existing) }
            if let challenge = existing.challenge, !existing.registrationId.isEmpty {
                return try await confirm(challenge: challenge)
            }
            return true
            }
        }

        if let previous = try pending() { try retire(previous) }
        if let active = try route() {
            try enqueue(active)
            try secure.delete(account("route"))
        }
        let candidate = RelayWakeRoute(
            routeId: UUID().uuidString.lowercased(),
            manageCredential: try Self.randomCredential(),
            wakeCredential: try Self.randomCredential(),
            tokenFingerprint: fingerprint
        )
        let generation = UUID()
        try save(PendingRegistration(registrationId: "", route: candidate, identity: identity, generation: generation, challenge: nil, confirmed: false, createdAt: now(), retry: nil))
        await retryCleanup()

        let response: HTTPResponse
        do { response = try await request("POST", "/v1/registrations", ["provider": "apns", "device_token": deviceToken]) }
        catch { try? scheduleRetry(generation); throw error }
        if isTerminalConfirmStatus(response.status) {
            if let current = try pending(), current.generation == generation { try retire(current); try secure.delete(account("pending")) }
            return false
        }
        guard response.status == 201, let body = try json(response.body),
              let registrationId = body["registration_id"] as? String, Self.uuid(registrationId) != nil,
              var stored = try pending(), stored.generation == generation, stored.identity == identity else { return false }
        stored = PendingRegistration(registrationId: registrationId, route: stored.route, identity: stored.identity, generation: stored.generation, challenge: stored.challenge, confirmed: stored.confirmed, createdAt: stored.createdAt, retry: nil)
        try save(stored)
        if let challenge = stored.challenge { return try await confirm(challenge: challenge) }
        return true
    }

    /// Stores every challenge before issuing confirm. This includes challenges that arrive after
    /// registration and makes a lost confirm response replay the immutable candidate proof.
    @discardableResult public func confirm(challenge: String) async throws -> Bool {
        guard Self.secret(challenge), var state = try pending(), state.identity == identity else { return false }
        state.challenge = challenge
        try save(state)
        guard !state.registrationId.isEmpty else { return true }
        if state.confirmed { return try await publishConfirmed(state) }

        var body: [String: Any] = ["challenge": challenge]
        if supportsCandidates {
            body["route_id"] = state.route.routeId
            body["manage_credential"] = state.route.manageCredential
            body["wake_credential"] = state.route.wakeCredential
        }
        let response: HTTPResponse
        do { response = try await request("POST", "/v1/registrations/\(state.registrationId)/confirm", body) }
        catch { try? scheduleRetry(state.generation); throw error }
        if isTerminalConfirmStatus(response.status) {
            if let current = try pending(), current.generation == state.generation { try retire(current); try secure.delete(account("pending")) }
            return false
        }
        guard response.status == 201, let value = try json(response.body) else {
            try scheduleRetry(state.generation)
            return false
        }
        if supportsCandidates {
            guard value["route_id"] as? String == state.route.routeId,
                  value["manage_credential"] as? String == state.route.manageCredential,
                  value["wake_credential"] as? String == state.route.wakeCredential else { return false }
        }
        // Legacy relays generate credentials; accept their opaque route only when all values are valid.
        let confirmedRoute: RelayWakeRoute
        if supportsCandidates {
            confirmedRoute = state.route
        } else {
            guard let routeId = value["route_id"] as? String, Self.uuid(routeId) != nil,
                  let manage = value["manage_credential"] as? String, Self.secret(manage),
                  let wake = value["wake_credential"] as? String, Self.secret(wake) else { return false }
            confirmedRoute = RelayWakeRoute(routeId: routeId, manageCredential: manage, wakeCredential: wake, tokenFingerprint: state.route.tokenFingerprint)
        }
        guard var current = try pending(), current.generation == state.generation, current.challenge == challenge else {
            try enqueue(confirmedRoute)
            return false
        }
        current = PendingRegistration(registrationId: current.registrationId, route: confirmedRoute, identity: current.identity, generation: current.generation, challenge: current.challenge, confirmed: true, createdAt: current.createdAt, retry: nil)
        try save(current)
        try secure.upsert(account("route"), try JSONEncoder().encode(confirmedRoute))
        return try await publishConfirmed(current)
    }

    @discardableResult public func publishConfirmedRoute() async throws -> Bool {
        if let state = try pending(), state.confirmed { return try await publishConfirmed(state) }
        guard let active = try route() else { return false }
        return try await publish(route: active)
    }

    public func route() throws -> RelayWakeRoute? {
        guard let data = try secure.read(account("route")) else { return nil }
        return try? JSONDecoder().decode(RelayWakeRoute.self, from: data)
    }

    public func registrationStatus() throws -> RegistrationStatus {
        if let state = try pending() {
            return RegistrationStatus(phase: state.confirmed ? .confirmedUnpublished : .pending, nextRetryAt: state.retry?.nextRetryAt)
        }
        return RegistrationStatus(phase: try route() == nil ? .idle : .registered, nextRetryAt: nil)
    }

    /// Lets the runtime retain its captured old-identity context until every durable cleanup
    /// operation has reached a terminal result. This accessor performs no network or mutation.
    public func hasPendingCleanup() throws -> Bool { !(try cleanups()).isEmpty }

    public func isBound(to expected: RelayWakeIdentity, relayOrigin: String) -> Bool {
        identity == expected && origin.serialized == relayOrigin
    }

    /// Queue first, then attempt independent server and relay cleanup. This works after the
    /// NativeSession has disconnected because the publisher owns captured identity credentials.
    public func revoke() async {
        if let active = try? route() {
            try? enqueue(active)
            try? secure.delete(account("route"))
        }
        if let pending = try? pending() { try? retire(pending) }
        try? secure.delete(account("pending"))
        await retryCleanup()
    }

    public func retryCleanup() async {
        guard let snapshot = try? cleanups() else { return }
        for item in snapshot {
            var updated = item
            if !item.serverRemoved {
                do { try await publisher.remove(routeId: item.route.routeId); updated.serverRemoved = true }
                catch { if isTerminalServerRemoval(error) { updated.serverRemoved = true } }
            }
            if !item.relayRemoved {
                do {
                    let response = try await request("POST", "/v1/routes/\(item.route.routeId)/revoke", ["manage_credential": item.route.manageCredential])
                    // A lost successful response is indistinguishable from relay's terminal no-authority reply.
                    if (200..<300).contains(response.status) || response.status == 401 || response.status == 404 { updated.relayRemoved = true }
                } catch { }
            }
            try? updateCleanup(updated)
        }
    }

    private func publishConfirmed(_ state: PendingRegistration) async throws -> Bool {
        guard state.confirmed else { return false }
        let published: Bool
        do { published = try await publish(route: state.route) }
        catch { try? scheduleRetry(state.generation); throw error }
        guard published, let current = try pending(), current.generation == state.generation, current.confirmed else { return published }
        try secure.delete(account("pending"))
        return true
    }

    private func publish(route: RelayWakeRoute) async throws -> Bool {
        try await publisher.publish(routeId: route.routeId, wakeCredential: route.wakeCredential)
        // An opt-out while publish was suspended wins. Requeue this exact route so a stale PUT
        // cannot leave a replacement UUID published.
        guard try self.route()?.routeId == route.routeId else {
            try enqueue(route, forceServerRetry: true)
            await retryCleanup()
            return false
        }
        return true
    }

    private func pending() throws -> PendingRegistration? {
        guard let data = try secure.read(account("pending")) else { return nil }
        return try? JSONDecoder().decode(PendingRegistration.self, from: data)
    }

    private func save(_ value: PendingRegistration) throws { try secure.upsert(account("pending"), try JSONEncoder().encode(value)) }
    private func isExpired(_ state: PendingRegistration, now: Date = Date()) -> Bool {
        guard let createdAt = state.createdAt else { return true }
        return now.timeIntervalSince(createdAt) > 10 * 60
    }
    private func scheduleRetry(_ generation: UUID, now: Date? = nil) throws {
        guard var state = try pending(), state.generation == generation else { return }
        let attempts = min(state.retry?.attempts ?? 0, 7) + 1
        // HTTPResponse does not expose headers, so Retry-After cannot be parsed at this boundary.
        let ceiling = min(300.0, pow(2.0, Double(attempts)) * 2)
        // Stable per-generation jitter avoids launch herds without storing another secret.
        let seed = generation.uuidString.utf8.reduce(0) { ($0 * 31 + Int($1)) % 250 }
        let salt = Double(seed) / 1_000
        state.retry = RetryState(attempts: attempts, nextRetryAt: (now ?? self.now()).addingTimeInterval(ceiling * (0.75 + salt)))
        try save(state)
    }
    private func cleanups() throws -> [Cleanup] {
        guard let data = try secure.read(account("cleanup")) else { return [] }
        return (try? JSONDecoder().decode([Cleanup].self, from: data)) ?? []
    }
    private func enqueue(_ route: RelayWakeRoute) throws {
        try enqueue(route, forceServerRetry: false)
    }
    private func enqueue(_ route: RelayWakeRoute, forceServerRetry: Bool) throws {
        var queue = try cleanups()
        if let index = queue.firstIndex(where: { $0.route.routeId == route.routeId }) {
            if forceServerRetry {
                queue[index].serverRemoved = false
                queue[index].revision = (queue[index].revision ?? 0) + 1
            }
        } else { queue.append(Cleanup(route: route, serverRemoved: false, relayRemoved: false, revision: nil)) }
        try saveCleanups(queue)
    }
    private func updateCleanup(_ updated: Cleanup) throws {
        var queue = try cleanups()
        guard let index = queue.firstIndex(where: { $0.route.routeId == updated.route.routeId }) else { return }
        // A stale publish can explicitly reopen server deletion while this retry was awaiting.
        guard queue[index].revision == updated.revision else { return }
        queue[index].serverRemoved = queue[index].serverRemoved || updated.serverRemoved
        queue[index].relayRemoved = queue[index].relayRemoved || updated.relayRemoved
        if queue[index].serverRemoved && queue[index].relayRemoved { queue.remove(at: index) }
        try saveCleanups(queue)
    }
    private func retire(_ state: PendingRegistration) throws {
        // A confirm may have committed before its response was lost; cleanup is idempotent.
        guard !state.registrationId.isEmpty, state.challenge != nil else { return }
        try enqueue(state.route)
    }
    private func isTerminalConfirmStatus(_ status: Int) -> Bool {
        (400..<500).contains(status) && status != 408 && status != 429
    }
    private func isTerminalServerRemoval(_ error: Error) -> Bool {
        switch ClientError.wrap(error) {
        case .unauthorized, .forbidden, .server(status: 401...403, code: _): return true
        default: return false
        }
    }
    private func saveCleanups(_ queue: [Cleanup]) throws {
        if queue.isEmpty { try secure.delete(account("cleanup")) }
        else { try secure.upsert(account("cleanup"), try JSONEncoder().encode(queue)) }
    }

    private func request(_ method: String, _ path: String, _ object: [String: Any]) async throws -> HTTPResponse {
        try Task.checkCancellation()
        let body = try JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes])
        let response = try await transport.send(HTTPRequest(method: method, url: origin.url(path), headers: ["Accept": "application/json", "Content-Type": "application/json"], body: body, maxResponseBytes: 16 * 1024))
        guard origin.contains(response.url) else { throw ClientError.originMismatch }
        return response
    }

    private func json(_ data: Data) throws -> [String: Any]? { (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] }
    private func account(_ name: String) -> String { "relay-wake-v2|\(identity.origin)|\(identity.vaultId)|\(identity.deviceId)|\(identity.environment)|\(origin.serialized)|\(name)" }
    private static func uuid(_ value: String) -> UUID? { UUID(uuidString: value).flatMap { $0.uuidString.lowercased() == value.lowercased() ? $0 : nil } }
    private static func secret(_ value: String) -> Bool { value.count == 64 && value.allSatisfy { $0.isHexDigit } }
    private static func fingerprint(_ token: String) -> String { SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined() }
    private static func randomCredential() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { throw ClientError.secureStorage(status: -1) }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }
}

/// Fences wake work that awaits across a preference change. Captured work may continue only while
/// the preference generation is unchanged and wakes remain enabled.
public struct WakeWorkFence: Sendable {
    public enum Checkpoint: Equatable, Sendable {
        /// Continue the captured work.
        case proceed
        /// Wakes are paused: retire and drain whatever this work may have made live, then stop.
        case retirePaused
        /// A newer preference generation owns the work now; stop without touching contexts.
        case superseded
    }

    public private(set) var generation: UInt = 0

    public init() {}

    public mutating func preferenceChanged() { generation &+= 1 }

    public func checkpoint(captured: UInt, enabled: Bool) -> Checkpoint {
        guard enabled else { return .retirePaused }
        return captured == generation ? .proceed : .superseded
    }
}

/// Runs only one main-actor wake operation for a matching key while allowing cancelled callers to
/// stop waiting without cancelling the shared operation.
@MainActor public final class CoalescedWakeWork<Key: Equatable> {
    private struct InFlight {
        let key: Key
        let token: UUID
        let task: Task<Void, Never>
    }

    private var inFlight: InFlight?

    public init() {}

    public func run(key: Key, _ work: @escaping @MainActor () async -> Void) async {
        let task: Task<Void, Never>
        if let inFlight, inFlight.key == key {
            task = inFlight.task
        } else {
            let token = UUID()
            task = Task { @MainActor [weak self] in
                await work()
                self?.complete(token: token)
            }
            inFlight = InFlight(key: key, token: token, task: task)
        }
        await waitForCompletion(of: task)
    }

    private func complete(token: UUID) {
        guard inFlight?.token == token else { return }
        inFlight = nil
    }

    private func waitForCompletion(of task: Task<Void, Never>) async {
        let waiter = WakeWorkWaiter()
        Task {
            await task.value
            waiter.resume()
        }
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                waiter.install(continuation)
            }
        } onCancel: {
            waiter.resume()
        }
    }
}

private final class WakeWorkWaiter: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?
    private var resumed = false

    func install(_ continuation: CheckedContinuation<Void, Never>) {
        lock.lock()
        if resumed {
            lock.unlock()
            continuation.resume()
            return
        }
        self.continuation = continuation
        lock.unlock()
    }

    func resume() {
        lock.lock()
        guard !resumed else {
            lock.unlock()
            return
        }
        resumed = true
        let continuation = continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume()
    }
}
