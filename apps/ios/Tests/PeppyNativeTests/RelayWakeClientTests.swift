import Testing
import Foundation
@testable import PeppyNative

@Suite struct RelayWakeClientTests {
    @Test func customSelectionRequiresExplicitOptInAndPersistedCanonicalOrigin() throws {
        let savedA = "https://relay-a.example.test"

        #expect(RelayWakeRegistrationTarget.custom(backgroundUpdatesEnabled: true, optedIn: false, persistedOrigin: savedA, environment: "production") == nil)
        #expect(RelayWakeRegistrationTarget.custom(backgroundUpdatesEnabled: true, optedIn: true, persistedOrigin: nil, environment: "production") == nil)
        #expect(RelayWakeRegistrationTarget.custom(backgroundUpdatesEnabled: true, optedIn: true, persistedOrigin: "https://relay-a.example.test/", environment: "production") == nil)

        let target = try #require(RelayWakeRegistrationTarget.custom(backgroundUpdatesEnabled: true, optedIn: true, persistedOrigin: savedA, environment: "production"))
        #expect(target.origin.serialized == savedA)
    }

    @Test func persistedCustomSelectionDoesNotUseTypedOriginAndHostedSelectionRemainsAutomatic() throws {
        let savedA = "https://relay-a.example.test"
        let typedB = "https://relay-b.example.test"

        let custom = try #require(RelayWakeRegistrationTarget.custom(backgroundUpdatesEnabled: true, optedIn: true, persistedOrigin: savedA, environment: "production"))
        #expect(custom.origin.serialized == savedA)
        #expect(custom.origin.serialized != typedB)

        let hosted = try #require(RelayWakeRegistrationTarget.hosted(configuration: HostedWakeConfiguration(relayOrigin: "https://hosted-relay.example.test", environment: "production")))
        #expect(hosted.origin.serialized == "https://hosted-relay.example.test")
        #expect(hosted.environment == "production")
    }

    @Test func confirmationStoresManageCredentialAndPublishesOnlyWakeRoute() async throws {
        let secure = InMemorySecureStore()
        let published = PublishedRoute()
        let transport = RelayTransport { request in
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: published)
        #expect(try await client.beginEnrollment(deviceToken: "apns-token"))
        #expect(try await client.confirm(challenge: String(repeating: "f", count: 64)))
        let publishedRoute = await published.value
        let route = try await client.route()
        #expect(publishedRoute?.0 == route?.routeId)
        #expect(publishedRoute?.1 == route?.wakeCredential)
        #expect(route?.manageCredential != nil)
    }

    @Test func coldRestartReusesConfirmedRouteWithoutRegistration() async throws {
        let secure = InMemorySecureStore()
        let published = PublishedRoute()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let origin = try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false)
        let first = RelayWakeClient(origin: origin, transport: transport, secure: secure, publisher: published)
        #expect(try await first.beginEnrollment(deviceToken: "token"))
        #expect(try await first.confirm(challenge: String(repeating: "f", count: 64)))
        let restarted = RelayWakeClient(origin: origin, transport: transport, secure: secure, publisher: published)
        #expect(try await restarted.beginEnrollment(deviceToken: "token"))
        #expect(requests.registrationCount == 1)
    }

    @Test func legacyRelayOmitsCandidateFieldsAndAcceptsServerRoute() async throws {
        let secure = InMemorySecureStore()
        let published = PublishedRoute()
        let transport = RelayTransport { request in
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            #expect(body["route_id"] == nil)
            return response(request, 201, ["route_id": "00000000-0000-4000-8000-000000000002", "manage_credential": String(repeating: "a", count: 64), "wake_credential": String(repeating: "b", count: 64)])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: published, supportsCandidates: false)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        #expect(try await client.confirm(challenge: String(repeating: "f", count: 64)))
        #expect(try await client.route()?.routeId == "00000000-0000-4000-8000-000000000002")
    }

    @Test func multipleRotatedRoutesRemainQueuedUntilEachCleanupSucceeds() async throws {
        let secure = InMemorySecureStore()
        let publisher = CleanupPublisher()
        let transport = RelayTransport { request in
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            if request.url.path.hasSuffix("/revoke") { return response(request, 204, [:]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        let challenge = String(repeating: "f", count: 64)
        #expect(try await client.beginEnrollment(deviceToken: "one"))
        #expect(try await client.confirm(challenge: challenge))
        #expect(try await client.beginEnrollment(deviceToken: "two"))
        #expect(try await client.confirm(challenge: challenge))
        #expect(try await client.beginEnrollment(deviceToken: "three"))
        await publisher.allowRemovals()
        await client.retryCleanup()
        let removed = await publisher.removedCount
        #expect(removed == 2)
    }

    @Test func rateLimitedConfirmPersistsRetryAndReplaysSameCandidate() async throws {
        let secure = InMemorySecureStore()
        let publisher = PublishedRoute()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            if requests.confirmCount == 1 { return response(request, 429, [:]) }
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        let challenge = String(repeating: "f", count: 64)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        #expect(!(try await client.confirm(challenge: challenge)))
        let retrying = try await client.registrationStatus()
        #expect(retrying.phase == .pending)
        #expect(retrying.nextRetryAt != nil)
        #expect(try await client.confirm(challenge: challenge))
        #expect(requests.registrationCount == 1)
        #expect(requests.confirmCount == 2)
        #expect(requests.confirmRouteIDs.count == 2)
        #expect(requests.confirmRouteIDs[0] == requests.confirmRouteIDs[1])
    }

    @Test func lostConfirmTransportResponseReplaysExactPersistedProof() async throws {
        let secure = InMemorySecureStore()
        let publisher = PublishedRoute()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            if requests.confirmCount == 1 { throw ClientError.unexpected("connection lost") }
            let body = try JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        let challenge = String(repeating: "f", count: 64)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        await #expect(throws: ClientError.self) { try await client.confirm(challenge: challenge) }
        #expect(try await client.confirm(challenge: challenge))
        #expect(requests.registrationCount == 1)
        #expect(requests.confirmRouteIDs.count == 2)
        #expect(requests.confirmRouteIDs[0] == requests.confirmRouteIDs[1])
    }

    @Test func lostPublishResponseRetainsConfirmedRouteForRetryWithoutRegistration() async throws {
        let secure = InMemorySecureStore()
        let publisher = FailingPublishPublisher()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        await #expect(throws: ClientError.self) { try await client.confirm(challenge: String(repeating: "f", count: 64)) }
        let pending = try await client.registrationStatus()
        #expect(pending.phase == .confirmedUnpublished)
        await publisher.allowPublish()
        #expect(try await client.publishConfirmedRoute())
        #expect(requests.registrationCount == 1)
        #expect((try await client.registrationStatus()).phase == .registered)
    }

    @Test func revokeDuringSuspendedConfirmCannotResurrectRoute() async throws {
        let secure = InMemorySecureStore()
        let publisher = CleanupPublisher()
        let transport = SuspendedConfirmTransport()
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        let confirmation = Task { try await client.confirm(challenge: String(repeating: "f", count: 64)) }
        await transport.waitForConfirm()
        await client.revoke()
        await transport.completeConfirm()
        #expect(!(try await confirmation.value))
        #expect(try await client.route() == nil)
        #expect((try await client.registrationStatus()).phase == .idle)
        #expect(try await client.hasPendingCleanup())
        await publisher.allowRemovals()
        await client.retryCleanup()
        let removed = await publisher.removedCount
        #expect(removed == 1)
        #expect(!(try await client.hasPendingCleanup()))
    }

    @Test func staleSuspendedConfirmCannotEraseReplacementPendingCandidate() async throws {
        let secure = InMemorySecureStore()
        let publisher = CountingPublisher()
        let transport = SuspendedConfirmTransport()
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        #expect(try await client.beginEnrollment(deviceToken: "first"))
        let confirmation = Task { try await client.confirm(challenge: String(repeating: "f", count: 64)) }
        await transport.waitForConfirm()
        #expect(try await client.beginEnrollment(deviceToken: "replacement"))
        await transport.completeConfirm()
        #expect(!(try await confirmation.value))
        #expect((try await client.registrationStatus()).phase == .pending)
        #expect(try await client.route() == nil)
    }

    @Test func earlyChallengeIsPersistedAndConfirmedAfterRegistrationResponse() async throws {
        let secure = InMemorySecureStore()
        let publisher = CountingPublisher()
        let transport = SuspendedRegistrationTransport()
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: publisher)
        let enrollment = Task { try await client.beginEnrollment(deviceToken: "token") }
        await transport.waitForRegistration()
        #expect(try await client.confirm(challenge: String(repeating: "f", count: 64)))
        await transport.completeRegistration()
        #expect(try await enrollment.value)
        #expect((try await client.registrationStatus()).phase == .registered)
        let publishes = await publisher.publishCount
        #expect(publishes == 1)
    }

    @Test func coldRestartDrainsDurableCleanupQueue() async throws {
        let secure = InMemorySecureStore()
        let cleanup = CleanupPublisher()
        let transport = RelayTransport { request in
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            if request.url.path.hasSuffix("/revoke") { return response(request, 204, [:]) }
            let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
            return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
        }
        let origin = try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false)
        let first = RelayWakeClient(origin: origin, transport: transport, secure: secure, publisher: cleanup)
        #expect(try await first.beginEnrollment(deviceToken: "token"))
        #expect(try await first.confirm(challenge: String(repeating: "f", count: 64)))
        await first.revoke()
        #expect(try await first.hasPendingCleanup())
        await cleanup.allowRemovals()
        let restarted = RelayWakeClient(origin: origin, transport: transport, secure: secure, publisher: cleanup)
        await restarted.retryCleanup()
        let removed = await cleanup.removedCount
        #expect(removed == 1)
        #expect(!(try await restarted.hasPendingCleanup()))
    }

    @Test func expiredRegistrationWithIdentifierReregistersInsteadOfPinningToken() async throws {
        let clock = TestClock()
        let secure = InMemorySecureStore()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: PublishedRoute(), now: { clock.value })
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        clock.advance(seconds: 601)
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        #expect(requests.registrationCount == 2)
    }

    @Test func terminalConfirmRejectionRetiresCandidateAndAllowsFreshRegistration() async throws {
        let secure = InMemorySecureStore()
        let requests = RelayRequests()
        let transport = RelayTransport { request in
            requests.record(request)
            if request.url.path == "/v1/registrations" { return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]) }
            if request.url.path.hasSuffix("/revoke") { return response(request, 204, [:]) }
            return response(request, 410, [:])
        }
        let client = RelayWakeClient(origin: try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false), transport: transport, secure: secure, publisher: PublishedRoute())
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        #expect(!(try await client.confirm(challenge: String(repeating: "f", count: 64))))
        #expect(try await client.beginEnrollment(deviceToken: "token"))
        #expect(requests.registrationCount == 2)
    }

    @Test func cleanupRetryPreservesEntryAppendedWhileRemovalIsSuspended() async throws {
        let secure = InMemorySecureStore()
        let origin = try ServerOrigin(canonical: FakeServer.origin, allowLoopbackHTTP: false)
        let first = RelayWakeRoute(routeId: "00000000-0000-4000-8000-000000000011", manageCredential: String(repeating: "a", count: 64), wakeCredential: String(repeating: "b", count: 64), tokenFingerprint: "first")
        let second = RelayWakeRoute(routeId: "00000000-0000-4000-8000-000000000012", manageCredential: String(repeating: "c", count: 64), wakeCredential: String(repeating: "d", count: 64), tokenFingerprint: "second")
        try secure.upsert(cleanupAccount(origin), cleanupData([first]))
        let publisher = BlockingCleanupPublisher()
        let client = RelayWakeClient(origin: origin, transport: RelayTransport { response($0, 204, [:]) }, secure: secure, publisher: publisher)
        let retry = Task { await client.retryCleanup() }
        await publisher.waitForFirstRemoval()
        try secure.upsert(cleanupAccount(origin), cleanupData([first, second]))
        await publisher.releaseFirstRemoval()
        await retry.value
        #expect(try await client.hasPendingCleanup())
        await client.retryCleanup()
        #expect(!(try await client.hasPendingCleanup()))
    }
}

private struct RelayTransport: HTTPTransport {
    let handler: @Sendable (HTTPRequest) throws -> HTTPResponse
    func send(_ request: HTTPRequest) async throws -> HTTPResponse { try handler(request) }
}
private actor PublishedRoute: RelayWakeRoutePublisher {
    var value: (String, String)?
    func publish(routeId: String, wakeCredential: String) async throws { value = (routeId, wakeCredential) }
    func remove(routeId: String) async throws {}
}
private actor CleanupPublisher: RelayWakeRoutePublisher {
    private var shouldFail = true
    private(set) var removedCount = 0
    func publish(routeId: String, wakeCredential: String) async throws {}
    func remove(routeId: String) async throws {
        if shouldFail { throw ClientError.unexpected("offline") }
        removedCount += 1
    }
    func allowRemovals() { shouldFail = false }
}
private actor FailingPublishPublisher: RelayWakeRoutePublisher {
    private var fail = true
    func publish(routeId: String, wakeCredential: String) async throws {
        if fail { throw ClientError.unexpected("offline") }
    }
    func remove(routeId: String) async throws {}
    func allowPublish() { fail = false }
}
private actor CountingPublisher: RelayWakeRoutePublisher {
    private(set) var publishCount = 0
    func publish(routeId: String, wakeCredential: String) async throws { publishCount += 1 }
    func remove(routeId: String) async throws {}
}
private actor BlockingCleanupPublisher: RelayWakeRoutePublisher {
    private var first = true
    private var removal: CheckedContinuation<Void, Never>?
    private var waiter: CheckedContinuation<Void, Never>?
    func publish(routeId: String, wakeCredential: String) async throws {}
    func remove(routeId: String) async throws {
        guard first else { return }
        first = false
        waiter?.resume()
        waiter = nil
        await withCheckedContinuation { removal = $0 }
    }
    func waitForFirstRemoval() async {
        guard !first else { return await withCheckedContinuation { waiter = $0 } }
    }
    func releaseFirstRemoval() { removal?.resume(); removal = nil }
}
private actor SuspendedConfirmTransport: HTTPTransport {
    private var confirm: CheckedContinuation<HTTPResponse, Error>?
    private var waiter: CheckedContinuation<Void, Never>?
    private var suspendedRequest: HTTPRequest?

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        if request.url.path == "/v1/registrations" {
            return response(request, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"])
        }
        if request.url.path.contains("/confirm") {
            suspendedRequest = request
            waiter?.resume()
            waiter = nil
            return try await withCheckedThrowingContinuation { confirm = $0 }
        }
        return response(request, 204, [:])
    }

    func waitForConfirm() async {
        guard suspendedRequest == nil else { return }
        await withCheckedContinuation { waiter = $0 }
    }

    func completeConfirm() {
        guard let request = suspendedRequest, let confirm else { return }
        let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
        self.confirm = nil
        confirm.resume(returning: response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!]))
    }
}
private actor SuspendedRegistrationTransport: HTTPTransport {
    private var registration: CheckedContinuation<HTTPResponse, Error>?
    private var waiter: CheckedContinuation<Void, Never>?
    private var registrationRequest: HTTPRequest?

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        if request.url.path == "/v1/registrations" {
            registrationRequest = request
            waiter?.resume()
            waiter = nil
            return try await withCheckedThrowingContinuation { registration = $0 }
        }
        let body = try! JSONSerialization.jsonObject(with: request.body!) as! [String: String]
        return response(request, 201, ["route_id": body["route_id"]!, "manage_credential": body["manage_credential"]!, "wake_credential": body["wake_credential"]!])
    }

    func waitForRegistration() async {
        guard registrationRequest == nil else { return }
        await withCheckedContinuation { waiter = $0 }
    }

    func completeRegistration() {
        guard let registrationRequest, let registration else { return }
        self.registration = nil
        registration.resume(returning: response(registrationRequest, 201, ["registration_id": "00000000-0000-4000-8000-000000000001"]))
    }
}
private final class RelayRequests: @unchecked Sendable {
    private let lock = NSLock()
    private var registrations = 0
    private var confirms = 0
    private var routeIDs: [String] = []
    var registrationCount: Int { lock.withLock { registrations } }
    var confirmCount: Int { lock.withLock { confirms } }
    var confirmRouteIDs: [String] { lock.withLock { routeIDs } }
    func record(_ request: HTTPRequest) {
        lock.withLock {
            if request.url.path == "/v1/registrations" { registrations += 1 }
            if request.url.path.contains("/confirm") {
                confirms += 1
                if let body = request.body,
                   let object = try? JSONSerialization.jsonObject(with: body) as? [String: String],
                   let routeID = object["route_id"] { routeIDs.append(routeID) }
            }
        }
    }
}
private final class TestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current = Date(timeIntervalSinceReferenceDate: 1_000)
    var value: Date { lock.withLock { current } }
    func advance(seconds: TimeInterval) { lock.withLock { current = current.addingTimeInterval(seconds) } }
}
private func cleanupAccount(_ origin: ServerOrigin) -> String {
    "relay-wake-v2|legacy|legacy|legacy|production|\(origin.serialized)|cleanup"
}
private func cleanupData(_ routes: [RelayWakeRoute]) -> Data {
    try! JSONSerialization.data(withJSONObject: routes.map {
        ["route": ["routeId": $0.routeId, "manageCredential": $0.manageCredential, "wakeCredential": $0.wakeCredential, "tokenFingerprint": $0.tokenFingerprint], "serverRemoved": false, "relayRemoved": false]
    })
}
private func response(_ request: HTTPRequest, _ status: Int, _ body: [String: Any]) -> HTTPResponse {
    HTTPResponse(status: status, url: request.url, body: try! JSONSerialization.data(withJSONObject: body))
}
