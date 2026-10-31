import Foundation

public enum ServerReachabilityState: Equatable, Sendable { case unknown, available, unavailable }

public struct ServerReachabilityBinding: Equatable, Sendable {
    public let origin: String
    public let vaultID: String
    public let deviceID: String

    public init(identity: EnrollmentIdentity) {
        origin = identity.origin
        vaultID = identity.vaultId
        deviceID = identity.deviceId
    }
}

final class ServerReachability: @unchecked Sendable {
    private struct Storage: Sendable {
        var binding: ServerReachabilityBinding?
        var generation = 0
        var state: ServerReachabilityState = .unknown
    }

    private let lock = NSLock()
    private var storage = Storage()

    var binding: ServerReachabilityBinding? { withLock { $0.binding } }
    var generation: Int { withLock { $0.generation } }
    var state: ServerReachabilityState { withLock { $0.state } }

    func bind(_ identity: EnrollmentIdentity?) {
        let next = identity.map(ServerReachabilityBinding.init)
        withLock { storage in
            guard storage.binding != next else { return }
            storage.binding = next
            storage.generation += 1
            storage.state = .unknown
        }
    }

    func record(_ outcome: ServerReachabilityState, binding: ServerReachabilityBinding, generation: Int) {
        withLock { storage in
            guard storage.binding == binding, storage.generation == generation else { return }
            storage.state = outcome
        }
    }

    private func withLock<T>(_ body: (inout Storage) -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return body(&storage)
    }
}

/// Session-scoped protocol header learned only from a generated canonical retention request.
final class AttachmentRetentionHTTPState: @unchecked Sendable {
    enum Negotiation { case unknown, supported, legacy }
    private let lock = NSLock()
    private var header: (name: String, value: String)?
    private var negotiation: Negotiation = .unknown
    private var readyAdvertised = false
    private var openGeneration: UInt64 = 0

    func remember(name: String, value: String) { lock.withLock { header = (name, value) } }

    func apply(to headers: inout [String: String], method: String) {
        guard method != "GET", let value = lock.withLock({ header }) else { return }
        headers[value.name] = value.value
    }

    func negotiationState() -> Negotiation { lock.withLock { negotiation } }
    func markSupported() { lock.withLock { negotiation = .supported } }
    func markLegacy() { lock.withLock { negotiation = .legacy } }
    func shouldAdvertiseReady() -> Bool { lock.withLock { !readyAdvertised } }
    func markReadyAdvertised() { lock.withLock { readyAdvertised = true } }
    func invalidateReadyAdvertisement() { lock.withLock { readyAdvertised = false } }
    func generation() -> UInt64 { lock.withLock { openGeneration } }
    func isCurrent(generation: UInt64) -> Bool { lock.withLock { openGeneration == generation } }
    func reset() {
        lock.withLock {
            header = nil
            negotiation = .unknown
            readyAdvertised = false
            openGeneration &+= 1
        }
    }
}

/// Gives foreground retry loops stable ownership so a cancelled predecessor cannot clear a replacement.
final class ForegroundReachabilityLoopOwnership {
    private var nextToken = 0
    private var activeToken: Int?

    func start() -> Int {
        nextToken += 1
        activeToken = nextToken
        return nextToken
    }

    func stop() { activeToken = nil }

    func finish(_ token: Int) {
        guard activeToken == token else { return }
        activeToken = nil
    }

    func isCurrent(_ token: Int) -> Bool { activeToken == token }
}

enum ServerResponseReachability {
    static func state(for status: Int) -> ServerReachabilityState {
        [502, 503, 504].contains(status) ? .unavailable : .available
    }

    static func state(for error: Error) -> ServerReachabilityState? {
        switch ClientError.wrap(error) {
        case .network: .unavailable
        default: nil
        }
    }
}

/// The shared request allowance of one operation. Every server call spends one request.
struct RequestMeter: Sendable {
    let limit: Int
    private(set) var used = 0

    init(limit: Int) { self.limit = limit }

    var exhausted: Bool { used >= limit }

    mutating func spend() throws {
        guard used < limit else { throw ClientError.budgetExhausted }
        used += 1
    }
}

/// Bearer-authenticated JSON calls bound to one origin. Paths are fixed by this module.
/// Its descriptions never include the token.
struct ServerClient: Sendable, CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
    static let maxVaultBytes = 4 * 1024 * 1024
    /// Server pages hold at most 8 MiB of envelopes plus at least one row.
    static let maxPageBytes = 12 * 1024 * 1024
    static let maxSmallBytes = 64 * 1024
    static let maxEnvelopeBytes = 4 * 1024 * 1024

    let origin: ServerOrigin
    let token: String
    let transport: any HTTPTransport
    let observeResponse: (@Sendable (ServerReachabilityState) -> Void)?
    let attachmentRetention: AttachmentRetentionHTTPState?

    init(
        origin: ServerOrigin,
        token: String,
        transport: any HTTPTransport,
        observeResponse: (@Sendable (ServerReachabilityState) -> Void)? = nil,
        attachmentRetention: AttachmentRetentionHTTPState? = nil
    ) {
        self.origin = origin
        self.token = token
        self.transport = transport
        self.observeResponse = observeResponse
        self.attachmentRetention = attachmentRetention
    }

    var description: String { "ServerClient(\(origin), token <redacted>)" }
    var debugDescription: String { description }
    var customMirror: Mirror { Mirror(self, children: ["origin": origin, "token": "<redacted>"]) }

    private func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        do {
            let response = try await transport.send(request)
            return response
        } catch {
            if let state = ServerResponseReachability.state(for: error) { observeResponse?(state) }
            throw error
        }
    }

    func get(_ path: String, query: [URLQueryItem] = [], limit: Int, meter: inout RequestMeter) async throws -> sending [String: Any] {
        try await call("GET", path, query: query, body: nil, limit: limit, meter: &meter)
    }

    func post(_ path: String, json: Data, limit: Int = maxSmallBytes, preserveCodedForbidden: Bool = false, meter: inout RequestMeter) async throws -> sending [String: Any] {
        guard json.count <= Self.maxEnvelopeBytes else { throw ClientError.requestTooLarge(limit: Self.maxEnvelopeBytes) }
        return try await call("POST", path, query: [], body: json, limit: limit, preserveCodedForbidden: preserveCodedForbidden, meter: &meter)
    }

    /// Sends an authenticated POST whose successful response is intentionally bodyless.
    func postNoContent(_ path: String, meter: inout RequestMeter) async throws {
        try Task.checkCancellation()
        try meter.spend()
        let response = try await send(HTTPRequest(
            method: "POST", url: origin.url(path), headers: mutationHeaders(),
            body: nil, maxResponseBytes: Self.maxSmallBytes
        ))
        guard origin.contains(response.url) else { throw ClientError.originMismatch }
        observeResponse?(ServerResponseReachability.state(for: response.status))
        let object = (try? JSONSerialization.jsonObject(with: response.body)) as? [String: Any]
        switch response.status {
        case 200..<300: return
        case 401: throw ClientError.unauthorized
        case 403:
            // A JSON-coded 403 refuses this request only; a bodiless 403 stays fatal.
            if let code = object?["code"] as? String { throw ClientError.server(status: 403, code: code) }
            throw ClientError.forbidden
        default:
            throw ClientError.server(status: response.status, code: object?["code"] as? String)
        }
    }

    private func mutationHeaders() -> [String: String] {
        var headers = ["Authorization": "Bearer \(token)", "Accept": "application/json"]
        attachmentRetention?.apply(to: &headers, method: "POST")
        return headers
    }

    /// Authenticated request whose status the caller interprets (attachment transfer). Spends one
    /// request; 401/403 and origin violations still throw.
    func exchange(
        _ method: String, _ path: String, query: [URLQueryItem] = [], body: Data? = nil, contentType: String? = nil, extraHeaders: [String: String] = [:], limit: Int, meter: inout RequestMeter
    ) async throws -> HTTPResponse {
        try Task.checkCancellation()
        try meter.spend()
        var headers = ["Authorization": "Bearer \(token)", "Accept": "application/json"]
        if let contentType { headers["Content-Type"] = contentType }
        attachmentRetention?.apply(to: &headers, method: method)
        headers.merge(extraHeaders, uniquingKeysWith: { _, supplied in supplied })
        let response = try await send(HTTPRequest(
            method: method, url: origin.url(path, query: query), headers: headers, body: body, maxResponseBytes: limit
        ))
        guard origin.contains(response.url) else { throw ClientError.originMismatch }
        observeResponse?(ServerResponseReachability.state(for: response.status))
        switch response.status {
        case 401: throw ClientError.unauthorized
        case 403: throw ClientError.forbidden
        default: return response
        }
    }

    private func call(
        _ method: String, _ path: String, query: [URLQueryItem], body: Data?, limit: Int, preserveCodedForbidden: Bool = false, meter: inout RequestMeter
    ) async throws -> sending [String: Any] {
        try Task.checkCancellation()
        try meter.spend()
        var headers = ["Authorization": "Bearer \(token)", "Accept": "application/json"]
        if body != nil { headers["Content-Type"] = "application/json" }
        attachmentRetention?.apply(to: &headers, method: method)
        let response = try await send(HTTPRequest(
            method: method, url: origin.url(path, query: query), headers: headers, body: body, maxResponseBytes: limit
        ))
        guard origin.contains(response.url) else { throw ClientError.originMismatch }
        observeResponse?(ServerResponseReachability.state(for: response.status))
        let object = (try? JSONSerialization.jsonObject(with: response.body)) as? [String: Any]
        switch response.status {
        case 200..<300:
            guard let object else { throw ClientError.invalidResponse("\(path) body") }
            return object
        case 401: throw ClientError.unauthorized
        case 403:
            if preserveCodedForbidden, let code = object?["code"] as? String {
                throw ClientError.server(status: 403, code: code)
            }
            throw ClientError.forbidden
        case 409 where object?["code"] as? String == "resync_required":
            throw ClientError.resyncRequired(reason: object?["reason"] as? String ?? "unknown")
        default:
            throw ClientError.server(status: response.status, code: object?["code"] as? String)
        }
    }

    /// A bounded authenticated request used when the vault is locked and a sync pass cannot run.
    func probe(meter: inout RequestMeter) async throws {
        _ = try await get("/v1/capabilities", limit: Self.maxSmallBytes, meter: &meter)
    }
}

/// Typed accessors for server JSON; every failure is `ClientError.invalidResponse`.
extension Dictionary where Key == String, Value == Any {
    func string(_ key: String) throws -> String {
        guard let value = self[key] as? String else { throw ClientError.invalidResponse(key) }
        return value
    }

    /// Cursors and counts are decimal strings on the wire.
    func decimal(_ key: String) throws -> UInt64 {
        guard let value = UInt64(try string(key)) else { throw ClientError.invalidResponse(key) }
        return value
    }

    func optionalDecimal(_ key: String) throws -> UInt64? {
        if self[key] == nil || self[key] is NSNull { return nil }
        return try decimal(key)
    }

    func integer(_ key: String) throws -> Int {
        guard let number = self[key] as? NSNumber, !number.isBoolean,
              let value = Int(exactly: number.doubleValue) else { throw ClientError.invalidResponse(key) }
        return value
    }

    func objects(_ key: String) throws -> [[String: Any]] {
        guard let value = self[key] as? [[String: Any]] else { throw ClientError.invalidResponse(key) }
        return value
    }

    func object(_ key: String) throws -> [String: Any] {
        guard let value = self[key] as? [String: Any] else { throw ClientError.invalidResponse(key) }
        return value
    }

    func jsonData() throws -> Data {
        do { return try JSONSerialization.data(withJSONObject: self, options: [.withoutEscapingSlashes]) }
        catch { throw ClientError.invalidResponse("envelope") }
    }
}
