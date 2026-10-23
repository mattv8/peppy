import Testing
@testable import PeppyNative

private actor ControlledCompletion {
    private var didStart = false
    private var started: CheckedContinuation<Void, Never>?
    private var completion: CheckedContinuation<Void, Never>?

    func wait() async {
        didStart = true
        started?.resume()
        await withCheckedContinuation { completion = $0 }
    }

    func waitUntilStarted() async {
        if didStart { return }
        await withCheckedContinuation { started = $0 }
    }

    func release() {
        completion?.resume()
        completion = nil
    }
}

@Suite struct ServerReachabilityTests {
    @Test func onlyGatewayFailuresAndNetworkLossAreUnavailable() {
        #expect(ServerResponseReachability.state(for: 502) == .unavailable)
        #expect(ServerResponseReachability.state(for: 503) == .unavailable)
        #expect(ServerResponseReachability.state(for: 504) == .unavailable)
        #expect(ServerResponseReachability.state(for: 401) == .available)
        #expect(ServerResponseReachability.state(for: 422) == .available)
        #expect(ServerResponseReachability.state(for: ClientError.network(code: -1009)) == .unavailable)
        #expect(ServerResponseReachability.state(for: ClientError.invalidResponse("local")) == nil)
    }

    @Test func staleBindingObservationsCannotReplaceTheCurrentState() {
        let first = EnrollmentIdentity(origin: "https://one.example.test", vaultId: "vault-one", deviceId: "device-one")
        let second = EnrollmentIdentity(origin: "https://two.example.test", vaultId: "vault-two", deviceId: "device-two")
        let reachability = ServerReachability()

        reachability.bind(first)
        let firstGeneration = reachability.generation
        reachability.bind(second)
        reachability.record(.unavailable, binding: ServerReachabilityBinding(identity: first), generation: firstGeneration)

        #expect(reachability.state == .unknown)
        reachability.record(.available, binding: ServerReachabilityBinding(identity: second), generation: reachability.generation)
        #expect(reachability.state == .available)
    }

    @Test @MainActor func replacingACancelledLoopKeepsTheReplacementOwned() async {
        let ownership = ForegroundReachabilityLoopOwnership()
        let completion = ControlledCompletion()
        let first = ownership.start()
        let cancelledLoop = Task { @MainActor in
            await completion.wait()
            ownership.finish(first)
        }
        await completion.waitUntilStarted()

        cancelledLoop.cancel()
        ownership.stop()
        let replacement = ownership.start()
        await completion.release()
        await cancelledLoop.value

        #expect(ownership.isCurrent(replacement))
    }

    @Test func synchronousObserverPreservesResponseOrdering() {
        let identity = EnrollmentIdentity(origin: "https://one.example.test", vaultId: "vault-one", deviceId: "device-one")
        let reachability = ServerReachability()
        reachability.bind(identity)
        let binding = ServerReachabilityBinding(identity: identity)
        let generation = reachability.generation

        reachability.record(.unavailable, binding: binding, generation: generation)
        reachability.record(.available, binding: binding, generation: generation)

        #expect(reachability.state == .available)
    }
}
