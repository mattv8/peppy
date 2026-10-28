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
        #expect(await host(producer, ComputerTransportFake([nil])).cancel() == .failed("offline"))
        #expect(producer.cancelled == 1)
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

    init(origin: String = "https://vault.example.test") {
        prompt = .init(origin: origin, invitationID: "invite", promptDigest: "digest", requestJSON: "request")
    }

    func createRequest(qr: String) throws -> OwnerComputerPrompt { prompt }
    func consumeCreated(invitationID: String, promptDigest: String, response: String) throws { consumed.append((invitationID, promptDigest, response)) }
    func view() throws -> OwnerComputerApproval? { approval }
    func approve(invitationID: String, promptDigest: String, codesMatch: Bool, vaultKeyPhrase: String) throws -> String {
        phrases.append(vaultKeyPhrase)
        return "approval"
    }
    func cancel() throws { cancelled += 1 }
}

private final class ComputerTransportFake: OwnerComputerTransport, @unchecked Sendable {
    private var responses: [OwnerPhoneHTTPResponse?]
    private(set) var requests: [String] = []
    private(set) var sessions: [String?] = []
    private(set) var credentials: [String] = []
    private(set) var bodies: [String?] = []

    init(_ responses: [OwnerPhoneHTTPResponse?] = []) { self.responses = responses }

    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        requests.append("\(method) \(path)")
        sessions.append(accountSession)
        credentials.append(credential)
        bodies.append(body)
        return responses.isEmpty ? .init(status: 200, body: "created") : responses.removeFirst()
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
