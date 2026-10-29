import Foundation
import Testing
@testable import PeppyNative

@Suite struct OwnerComputerApprovalHostTests {
    private let identity = EnrollmentIdentity(origin: "https://vault.example.test", vaultId: "vault", deviceId: "device")

    private func host(
        _ producer: ComputerProducerFake = ComputerProducerFake(),
        _ transport: ComputerTransportFake = ComputerTransportFake(),
        _ secure: ComputerFaultyStore = ComputerFaultyStore(),
        isHosted: Bool = false,
        session: String? = nil
    ) -> OwnerComputerApprovalHost {
        OwnerComputerApprovalHost(
            producer: producer,
            transport: transport,
            secrets: VaultSecretStore(secure: secure),
            identity: identity,
            credential: "device",
            isHosted: isHosted,
            accountSession: session
        )
    }

    @Test func scanCreatesAndPinsTheLocalComparisonCode() async {
        let producer = ComputerProducerFake()
        #expect(await host(producer).scan(qr: "computer-qr") == .view(producer.approval))
        #expect(producer.consumed.count == 1)
    }

    @Test func storedPhraseApprovesWithoutPrompting() async {
        let secure = ComputerFaultyStore(); secure.seed(identity.account("vault-secret-v2"), "vault phrase")
        let producer = ComputerProducerFake(); let prompt = ComputerPrompt(["unused"])
        #expect(await host(producer, ComputerTransportFake(), secure).approve(codesMatch: true, prompt: { await prompt.ask() }) == .approved)
        #expect(prompt.asked == 0)
        #expect(producer.phrases == ["vault phrase"])
    }

    @Test func missingPhrasePromptsThenCachesVerifiedPhraseForRetry() async {
        let secure = ComputerFaultyStore(); let producer = ComputerProducerFake(); let prompt = ComputerPrompt(["vault phrase"])
        let transport = ComputerTransportFake([nil, .init(status: 200, body: "{}")])
        let approvalHost = host(producer, transport, secure)
        #expect(await approvalHost.approve(codesMatch: true, prompt: { await prompt.ask() }) == .failed("offline"))
        #expect(await approvalHost.approve(codesMatch: true, prompt: { await prompt.ask() }) == .approved)
        #expect(prompt.asked == 1)
        #expect(producer.phrases == ["vault phrase", "vault phrase"])
        #expect(secure.value(identity.account("vault-secret-v2")) == "vault phrase")
        #expect(transport.bodies == ["approval", "approval"], "a retry must retain the binding's exact approval body")
    }

    @Test func secureStoreReadFailureFailsClosedWithoutPrompt() async {
        let secure = ComputerFaultyStore(); secure.failReads = true
        let prompt = ComputerPrompt(["vault phrase"])
        #expect(await host(ComputerProducerFake(), ComputerTransportFake(), secure).approve(codesMatch: true, prompt: { await prompt.ask() }) == .failed("secure-store-unavailable"))
        #expect(prompt.asked == 0)
    }

    @Test func stalePromptAfterPhraseEntryNeverApproves() async {
        let producer = ComputerProducerFake(); let secure = ComputerFaultyStore()
        let result = await host(producer, ComputerTransportFake(), secure).approve(codesMatch: true, prompt: {
            producer.approval = .init(invitationID: "other", promptDigest: "changed", sas: "654321")
            return "vault phrase"
        })
        #expect(result == .cancelled)
        #expect(producer.phrases.isEmpty)
    }

    @Test func maliciousCrossOriginQRNeverReachesTransport() async {
        let producer = ComputerProducerFake(origin: "https://attacker.example")
        let transport = ComputerTransportFake()
        #expect(await host(producer, transport).scan(qr: "malicious") == .failed("invalid-computer-qr"))
        #expect(transport.requests.isEmpty)
        #expect(producer.cancelled == 1)
    }

    @Test func hostedApprovalRequiresAndForwardsTheAccountSessionWithDeviceCredential() async {
        let deniedTransport = ComputerTransportFake()
        #expect(await host(ComputerProducerFake(), deniedTransport, isHosted: true).scan(qr: "computer-qr") == .failed("account-required"))
        #expect(deniedTransport.requests.isEmpty)

        let transport = ComputerTransportFake()
        _ = await host(ComputerProducerFake(), transport, isHosted: true, session: "account-session").scan(qr: "computer-qr")
        #expect(transport.sessions == ["account-session"])
        #expect(transport.credentials == ["device"])
    }

    @Test func cancellationClearsLocalBindingStateEvenWhenOffline() async {
        let producer = ComputerProducerFake()
        #expect(await host(producer, ComputerTransportFake([nil])).cancel() == .cancelled)
        #expect(producer.cancelled == 1)
    }

    @Test func cancellationBeforeScanningSwallowsMissingPromptErrors() async {
        let producer = ComputerProducerFake(hasPrompt: false)
        let transport = ComputerTransportFake()

        #expect(await host(producer, transport).cancel() == .cancelled)
        #expect(producer.cancelAttempts == 1)
        #expect(transport.requests.isEmpty)
    }

    @Test func failedScanClearsThePromptBeforeRescanning() async {
        let producer = ComputerProducerFake(hasPrompt: false, rejectsActiveCreate: true)
        let transport = ComputerTransportFake([nil, .init(status: 200, body: "created")])
        let approvalHost = host(producer, transport)

        #expect(await approvalHost.scan(qr: "computer-qr") == .failed("offline"))
        #expect(await approvalHost.scan(qr: "computer-qr") == .view(producer.approval))
    }

    @Test func cancellationWaitsForAnInFlightApprovalBeforeCleaningUp() async {
        let pause = ComputerApprovalPause()
        let producer = ComputerProducerFake()
        let transport = ComputerTransportFake(pause: pause)
        let approvalHost = host(producer, transport)

        let approval = Task { await approvalHost.approve(codesMatch: true, prompt: { "vault phrase" }) }
        await pause.waitForApproval()
        let cancellation = Task {
            await pause.recordCancellationRequest()
            return await approvalHost.cancel()
        }
        await pause.waitForCancellationRequest()
        await Task.yield()
        #expect(transport.requests == ["POST /v1/enrollment/computers/invite/approve"])
        await pause.release()

        #expect(await approval.value == .approved)
        #expect(await cancellation.value == .cancelled)
        #expect(transport.requests == ["POST /v1/enrollment/computers/invite/approve"])
        #expect(producer.cancelled == 0)
    }

    @Test func failedApprovalThenQueuedCancellationDeletesTheInvitation() async {
        let pause = ComputerApprovalPause()
        let producer = ComputerProducerFake()
        let transport = ComputerTransportFake([.init(status: 200, body: "created"), nil], pause: pause)
        let approvalHost = host(producer, transport)
        #expect(await approvalHost.scan(qr: "computer-qr") == .view(producer.approval))

        let approval = Task { await approvalHost.approve(codesMatch: true, prompt: { "vault phrase" }) }
        await pause.waitForApproval()
        let cancellation = Task {
            await pause.recordCancellationRequest()
            return await approvalHost.cancel()
        }
        await pause.waitForCancellationRequest()
        await Task.yield()
        #expect(transport.requests == [
            "POST /v1/enrollment/computers",
            "POST /v1/enrollment/computers/invite/approve",
        ])
        await pause.release()

        #expect(await approval.value == .failed("offline"))
        #expect(await cancellation.value == .cancelled)
        #expect(transport.requests == [
            "POST /v1/enrollment/computers",
            "POST /v1/enrollment/computers/invite/approve",
            "DELETE /v1/enrollment/computers/invite",
        ])
    }

    @Test func approvedServerResponseIsNeverDeletedWhenTheLocalRecheckFails() async {
        let producer = ComputerProducerFake(failingViewCall: 4)
        let transport = ComputerTransportFake([.init(status: 200, body: "created"), .init(status: 200, body: "approved")])
        let approvalHost = host(producer, transport)
        #expect(await approvalHost.scan(qr: "computer-qr") == .view(producer.approval))

        #expect(await approvalHost.approve(codesMatch: true, prompt: { "vault phrase" }) == .failed("enrollment"))
        #expect(await approvalHost.cancel() == .cancelled)
        #expect(transport.requests == [
            "POST /v1/enrollment/computers",
            "POST /v1/enrollment/computers/invite/approve",
        ])
    }

    @Test func dismissalRunsTheSameCleanupAsCancellation() async {
        let producer = ComputerProducerFake()
        let transport = ComputerTransportFake()

        await host(producer, transport).dismiss()
        #expect(producer.cancelled == 1)
        #expect(transport.requests == ["DELETE /v1/enrollment/computers/invite"])
    }

    @Test func cancellationAfterApprovalNeverSendsDeleteOrClearsTheApprovedTranscript() async {
        let producer = ComputerProducerFake()
        let transport = ComputerTransportFake()
        let approvalHost = host(producer, transport)
        #expect(await approvalHost.approve(codesMatch: true, prompt: { "vault phrase" }) == .approved)
        #expect(await approvalHost.cancel() == .cancelled)
        await approvalHost.dismiss()
        #expect(transport.requests == ["POST /v1/enrollment/computers/invite/approve"])
        #expect(producer.cancelled == 0)
    }
}

private final class ComputerProducerFake: OwnerComputerProducer, @unchecked Sendable {
    let prompt: OwnerComputerPrompt
    var approval = OwnerComputerApproval(invitationID: "invite", promptDigest: "digest", sas: "123456")
    private(set) var consumed: [(String, String, String)] = []
    private(set) var phrases: [String] = []
    private(set) var cancelled = 0
    private(set) var cancelAttempts = 0
    private var hasPrompt: Bool
    private let rejectsActiveCreate: Bool
    private let failingViewCall: Int?
    private var viewCalls = 0

    init(origin: String = "https://vault.example.test", hasPrompt: Bool = true, rejectsActiveCreate: Bool = false, failingViewCall: Int? = nil) {
        prompt = .init(origin: origin, invitationID: "invite", promptDigest: "digest", requestJSON: "request")
        self.hasPrompt = hasPrompt
        self.rejectsActiveCreate = rejectsActiveCreate
        self.failingViewCall = failingViewCall
    }

    func createRequest(qr: String) throws -> OwnerComputerPrompt {
        guard !rejectsActiveCreate || !hasPrompt else { throw ClientError.invalidResponse("active prompt") }
        hasPrompt = true
        return prompt
    }
    func consumeCreated(invitationID: String, promptDigest: String, response: String) throws { consumed.append((invitationID, promptDigest, response)) }
    func view() throws -> OwnerComputerApproval? {
        viewCalls += 1
        if viewCalls == failingViewCall { throw ClientError.invalidResponse("recheck failed") }
        guard hasPrompt else { throw ClientError.invalidResponse("no prompt") }
        return approval
    }
    func approve(invitationID: String, promptDigest: String, codesMatch: Bool, vaultKeyPhrase: String) throws -> String {
        phrases.append(vaultKeyPhrase)
        return "approval"
    }
    func cancel() throws {
        cancelAttempts += 1
        guard hasPrompt else { throw ClientError.invalidResponse("no prompt") }
        hasPrompt = false
        cancelled += 1
    }
}

private final class ComputerTransportFake: OwnerComputerTransport, @unchecked Sendable {
    private var responses: [OwnerPhoneHTTPResponse?]
    private(set) var requests: [String] = []
    private(set) var sessions: [String?] = []
    private(set) var credentials: [String] = []
    private(set) var bodies: [String?] = []
    private let pause: ComputerApprovalPause?

    init(_ responses: [OwnerPhoneHTTPResponse?] = [], pause: ComputerApprovalPause? = nil) {
        self.responses = responses
        self.pause = pause
    }

    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        requests.append("\(method) \(path)")
        sessions.append(accountSession)
        credentials.append(credential)
        bodies.append(body)
        if method == "POST", path.hasSuffix("/approve") { await pause?.pauseApproval() }
        return responses.isEmpty ? .init(status: 200, body: "created") : responses.removeFirst()
    }
}

private actor ComputerApprovalPause {
    private var approvalStarted = false
    private var started: CheckedContinuation<Void, Never>?
    private var cancellationRequested = false
    private var cancellationRequest: CheckedContinuation<Void, Never>?
    private var releaseContinuation: CheckedContinuation<Void, Never>?

    func pauseApproval() async {
        approvalStarted = true
        started?.resume()
        started = nil
        await withCheckedContinuation { releaseContinuation = $0 }
    }

    func waitForApproval() async {
        guard !approvalStarted else { return }
        await withCheckedContinuation { started = $0 }
    }

    func recordCancellationRequest() {
        cancellationRequested = true
        cancellationRequest?.resume()
        cancellationRequest = nil
    }

    func waitForCancellationRequest() async {
        guard !cancellationRequested else { return }
        await withCheckedContinuation { cancellationRequest = $0 }
    }

    func release() {
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private final class ComputerPrompt: @unchecked Sendable {
    private var answers: [String?]
    private(set) var asked = 0
    init(_ answers: [String?]) { self.answers = answers }
    func ask() async -> String? { asked += 1; return answers.isEmpty ? nil : answers.removeFirst() }
}

private final class ComputerFaultyStore: SecureStore, @unchecked Sendable {
    private let store = InMemorySecureStore()
    var failReads = false
    func seed(_ account: String, _ value: String) { try? store.upsert(account, Data(value.utf8)) }
    func value(_ account: String) -> String? { store.value(account).flatMap { String(data: $0, encoding: .utf8) } }
    func read(_ account: String) throws(ClientError) -> Data? {
        if failReads { throw .unexpected("keychain read") }
        return store.value(account)
    }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool { try store.addIfAbsent(account, data) }
    func upsert(_ account: String, _ data: Data) throws(ClientError) { try store.upsert(account, data) }
    func delete(_ account: String) throws(ClientError) { try store.delete(account) }
}
