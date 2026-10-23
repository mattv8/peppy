import Foundation
import Testing
@testable import PeppyNative

private final class RecordingTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedRequests: [HTTPRequest] = []
    private let responder: @Sendable (HTTPRequest) -> HTTPResponse

    init(responder: @escaping @Sendable (HTTPRequest) -> HTTPResponse) {
        self.responder = responder
    }

    var requests: [HTTPRequest] { lock.withLock { recordedRequests } }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { recordedRequests.append(request) }
        return responder(request)
    }
}

private final class FailingRecordingTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedRequests: [HTTPRequest] = []

    var requests: [HTTPRequest] { lock.withLock { recordedRequests } }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { recordedRequests.append(request) }
        throw ClientError.network(code: URLError.notConnectedToInternet.rawValue)
    }
}

private struct CancellationProbe: Sendable {
    let used: Int
    let threwCancellation: Bool
}

/// Characterizes the authenticated ServerClient boundary without a real network.
@Suite struct ServerClientTests {
    let token = "test-device-token"

    func client(_ transport: any HTTPTransport) throws -> ServerClient {
        ServerClient(
            origin: try ServerOrigin(canonical: "https://push.example.test", allowLoopbackHTTP: false),
            token: token,
            transport: transport
        )
    }

    @Test func getPreservesQueryAuthorizationAndResponseLimit() async throws {
        let transport = RecordingTransport { request in
            HTTPResponse(status: 200, url: request.url, body: Data("{\"high_water_cursor\":\"0\"}".utf8))
        }
        var meter = RequestMeter(limit: 1)

        let response = try await client(transport).get(
            "/v1/events",
            query: [URLQueryItem(name: "after", value: "7"), URLQueryItem(name: "limit", value: "3")],
            limit: 123,
            meter: &meter
        )

        let request = try #require(transport.requests.first)
        #expect(response["high_water_cursor"] as? String == "0")
        #expect(request.method == "GET")
        #expect(request.url.path == "/v1/events")
        #expect(URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems == [
            URLQueryItem(name: "after", value: "7"), URLQueryItem(name: "limit", value: "3"),
        ])
        #expect(request.headers == ["Authorization": "Bearer \(token)", "Accept": "application/json"])
        #expect(request.body == nil)
        #expect(request.maxResponseBytes == 123)
    }

    @Test func postEncodesJSONBodyAndContentType() async throws {
        let transport = RecordingTransport { request in
            HTTPResponse(status: 200, url: request.url, body: Data("{\"cursor\":\"1\"}".utf8))
        }
        let body = Data("{\"envelope_id\":\"event-1\"}".utf8)
        var meter = RequestMeter(limit: 1)

        let response = try await client(transport).post("/v1/events", json: body, meter: &meter)

        let request = try #require(transport.requests.first)
        #expect(response["cursor"] as? String == "1")
        #expect(request.method == "POST")
        #expect(request.headers["Content-Type"] == "application/json")
        #expect(request.headers["Authorization"] == "Bearer \(token)")
        #expect(request.body == body)
        #expect(request.maxResponseBytes == ServerClient.maxSmallBytes)
    }

    @Test func exchangeReturnsNonAuthenticationStatusForCallerInterpretation() async throws {
        let transport = RecordingTransport { request in
            HTTPResponse(status: 404, url: request.url, body: Data("{\"code\":\"not_found\"}".utf8))
        }
        var meter = RequestMeter(limit: 1)

        let response = try await client(transport).exchange("DELETE", "/v1/attachments/missing", limit: 77, meter: &meter)

        let request = try #require(transport.requests.first)
        #expect(response.status == 404)
        #expect(request.method == "DELETE")
        #expect(request.maxResponseBytes == 77)
    }

    @Test func exchangeMapsAuthenticationStatusesInsteadOfReturningThem() async throws {
        let unauthorized = RecordingTransport { request in HTTPResponse(status: 401, url: request.url, body: Data()) }
        var unauthorizedMeter = RequestMeter(limit: 1)
        await #expect(throws: ClientError.unauthorized) {
            try await client(unauthorized).exchange("GET", "/v1/devices", limit: 1, meter: &unauthorizedMeter)
        }

        let forbidden = RecordingTransport { request in HTTPResponse(status: 403, url: request.url, body: Data()) }
        var forbiddenMeter = RequestMeter(limit: 1)
        await #expect(throws: ClientError.forbidden) {
            try await client(forbidden).exchange("GET", "/v1/devices", limit: 1, meter: &forbiddenMeter)
        }
    }

    @Test func exchangePreservesQueryAndObservesTransportFailure() async throws {
        let identity = EnrollmentIdentity(origin: "https://push.example.test", vaultId: "vault", deviceId: "device")
        let reachability = ServerReachability()
        reachability.bind(identity)
        let binding = ServerReachabilityBinding(identity: identity)
        let generation = reachability.generation
        let transport = FailingRecordingTransport()
        let client = ServerClient(
            origin: try ServerOrigin(canonical: "https://push.example.test", allowLoopbackHTTP: false),
            token: token,
            transport: transport,
            observeResponse: { reachability.record($0, binding: binding, generation: generation) }
        )
        var meter = RequestMeter(limit: 1)

        await #expect(throws: ClientError.network(code: URLError.notConnectedToInternet.rawValue)) {
            try await client.exchange(
                "GET",
                "/v1/attachments",
                query: [URLQueryItem(name: "cursor", value: "7")],
                limit: 1,
                meter: &meter
            )
        }

        let request = try #require(transport.requests.first)
        #expect(URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems == [
            URLQueryItem(name: "cursor", value: "7"),
        ])
        #expect(reachability.state == .unavailable)
    }

    @Test func postNoContentAcceptsEmptySuccessfulResponse() async throws {
        let transport = RecordingTransport { request in HTTPResponse(status: 204, url: request.url, body: Data()) }
        var meter = RequestMeter(limit: 1)

        try await client(transport).postNoContent("/v1/compaction/capability", meter: &meter)

        let request = try #require(transport.requests.first)
        #expect(request.method == "POST")
        #expect(request.body == nil)
        #expect(request.headers["Content-Type"] == nil)
    }

    @Test func probeUsesBoundedCapabilitiesRequestAndPublishesBeforeReturning() async throws {
        let identity = EnrollmentIdentity(origin: "https://push.example.test", vaultId: "vault", deviceId: "device")
        let reachability = ServerReachability()
        reachability.bind(identity)
        let binding = ServerReachabilityBinding(identity: identity)
        let generation = reachability.generation
        let transport = RecordingTransport { request in HTTPResponse(status: 503, url: request.url, body: Data("{}".utf8)) }
        let client = ServerClient(
            origin: try ServerOrigin(canonical: "https://push.example.test", allowLoopbackHTTP: false),
            token: token,
            transport: transport,
            observeResponse: { reachability.record($0, binding: binding, generation: generation) }
        )
        var meter = RequestMeter(limit: 1)

        await #expect(throws: ClientError.server(status: 503, code: nil)) {
            try await client.probe(meter: &meter)
        }

        let request = try #require(transport.requests.first)
        #expect(request.url.path == "/v1/capabilities")
        #expect(request.url.query == nil)
        #expect(request.maxResponseBytes == ServerClient.maxSmallBytes)
        #expect(meter.used == 1)
        #expect(reachability.state == .unavailable)
    }

    @Test func getDistinguishesResyncFallbackMalformedBodyAndForeignOrigin() async throws {
        let resync = RecordingTransport { request in
            HTTPResponse(status: 409, url: request.url, body: Data("{\"code\":\"resync_required\"}".utf8))
        }
        var resyncMeter = RequestMeter(limit: 1)
        await #expect(throws: ClientError.resyncRequired(reason: "unknown")) {
            try await client(resync).get("/v1/events", limit: 1, meter: &resyncMeter)
        }

        let malformed = RecordingTransport { request in
            HTTPResponse(status: 200, url: request.url, body: Data("[\"not an object\"]".utf8))
        }
        var malformedMeter = RequestMeter(limit: 1)
        await #expect(throws: ClientError.invalidResponse("/v1/events body")) {
            try await client(malformed).get("/v1/events", limit: 1, meter: &malformedMeter)
        }

        let foreignOrigin = RecordingTransport { _ in
            HTTPResponse(status: 200, url: URL(string: "https://wrong.example.test/v1/events"), body: Data("{}".utf8))
        }
        var originMeter = RequestMeter(limit: 1)
        await #expect(throws: ClientError.originMismatch) {
            try await client(foreignOrigin).get("/v1/events", limit: 1, meter: &originMeter)
        }
    }

    @Test func meterRejectsRequestBeforeSecondSend() async throws {
        let transport = RecordingTransport { request in HTTPResponse(status: 200, url: request.url, body: Data("{}".utf8)) }
        var meter = RequestMeter(limit: 1)

        _ = try await client(transport).get("/v1/snapshot", limit: 1, meter: &meter)
        await #expect(throws: ClientError.budgetExhausted) {
            try await client(transport).get("/v1/snapshot", limit: 1, meter: &meter)
        }

        #expect(transport.requests.count == 1)
    }

    @Test func cancellationBeforeCallDoesNotSpendOrSend() async throws {
        let transport = RecordingTransport { request in HTTPResponse(status: 200, url: request.url, body: Data("{}".utf8)) }
        let task = Task { () -> CancellationProbe in
            var meter = RequestMeter(limit: 1)
            withUnsafeCurrentTask { $0?.cancel() }
            do {
                _ = try await client(transport).get("/v1/snapshot", limit: 1, meter: &meter)
                return CancellationProbe(used: meter.used, threwCancellation: false)
            } catch is CancellationError {
                return CancellationProbe(used: meter.used, threwCancellation: true)
            } catch {
                return CancellationProbe(used: meter.used, threwCancellation: false)
            }
        }
        let probe = await task.value

        #expect(probe.threwCancellation)
        #expect(probe.used == 0)
        #expect(transport.requests.isEmpty)
    }
}
