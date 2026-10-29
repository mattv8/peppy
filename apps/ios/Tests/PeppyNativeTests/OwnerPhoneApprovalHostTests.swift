import Foundation
import Testing
@testable import PeppyNative

@Suite struct OwnerPhoneApprovalHostTests {
    private let identity = EnrollmentIdentity(origin: "https://vault.example.test", vaultId: "vault", deviceId: "device")
    private var account: String { identity.account("vault-secret-v2") }
    private let invitePath = "/v1/enrollment/invitations/invite"
    private let approvePath = "/v1/enrollment/invitations/invite/approve"

    private func host(
        _ producer: OwnerProducerFake = OwnerProducerFake(),
        _ transport: OwnerTransportFake = OwnerTransportFake(),
        _ secure: OwnerFaultyStore = OwnerFaultyStore(),
        isHosted: Bool = false,
        session: String? = nil
    ) -> OwnerPhoneApprovalHost {
        OwnerPhoneApprovalHost(
            producer: producer, transport: transport, secrets: VaultSecretStore(secure: secure),
            identity: identity, credential: "device", isHosted: isHosted, accountSession: session
        )
    }

    // MARK: Approval secret policy

    @Test func storedSecretApprovesWithoutPrompting() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake(); let prompt = PromptScript(["unused"])
        let result = await host(producer, transport, secure).approve(prompt: { await prompt.ask() })
        #expect(result == .view(approved(producer), qr: nil))
        #expect(prompt.asked == 0)
        #expect(producer.approvedSecrets == ["vault secret"])
        #expect(transport.requests == ["GET \(invitePath)", "POST \(approvePath)"])
    }

    @Test func missingSecretPromptsOnceSavesAndLaterApprovalsStaysilent() async {
        let secure = OwnerFaultyStore(); let producer = OwnerProducerFake(); let transport = OwnerTransportFake()
        let prompt = PromptScript(["vault secret"])
        let approver = host(producer, transport, secure)
        #expect(await approver.approve(prompt: { await prompt.ask() }) == .view(approved(producer), qr: nil))
        #expect(prompt.asked == 1)
        #expect(secure.stored(account) == "vault secret")

        // A second approval is a new invitation on the same origin; an approved invitation itself is never approved again.
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .view(approved(producer), qr: nil))
        #expect(prompt.asked == 1, "the saved secret must not prompt again")
        #expect(producer.approvedSecrets == ["vault secret", "vault secret"])
    }

    @Test func wrongPromptedSecretFailsWithoutSavingOrApproving() async {
        let secure = OwnerFaultyStore(); let producer = OwnerProducerFake(matches: false); let transport = OwnerTransportFake()
        let prompt = PromptScript(["wrong secret"])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .failed("wrong-passphrase"))
        #expect(secure.stored(account) == nil)
        #expect(producer.approvedSecrets.isEmpty)
        #expect(transport.requests == ["GET \(invitePath)", "GET \(invitePath)"], "the prompt path re-reads before verifying the secret")
    }

    @Test func storedSecretMismatchFailsWithoutPromptOrOverwrite() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "stale")
        let producer = OwnerProducerFake(matches: false); let transport = OwnerTransportFake(); let prompt = PromptScript(["fresh"])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .failed("vault-secret-mismatch"))
        #expect(prompt.asked == 0)
        #expect(secure.stored(account) == "stale")
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func promptCancelReturnsCancelledWithoutSavingOrApproving() async {
        let secure = OwnerFaultyStore(); let producer = OwnerProducerFake(); let transport = OwnerTransportFake()
        let prompt = PromptScript([nil])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .cancelled)
        #expect(prompt.asked == 1)
        #expect(secure.stored(account) == nil)
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func secureStoreReadFailureFailsClosedWithoutPrompting() async {
        let secure = OwnerFaultyStore(); secure.failReads = true
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake(); let prompt = PromptScript(["vault secret"])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .failed("secure-store-unavailable"))
        #expect(prompt.asked == 0)
        #expect(producer.approvedSecrets.isEmpty)
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func invitationIdChangedDuringPromptCancelsWithoutApproving() async {
        let secure = OwnerFaultyStore(); let producer = OwnerProducerFake(); let transport = OwnerTransportFake()
        let result = await host(producer, transport, secure).approve(prompt: {
            producer.value = .init(invitationId: "other", stage: .verify, expiresInSeconds: 30, sas: "123456", claimDigest: "digest")
            return "vault secret"
        })
        #expect(result == .cancelled)
        #expect(secure.stored(account) == nil)
        #expect(producer.approvedSecrets.isEmpty)
        #expect(transport.requests == ["GET \(invitePath)", "GET /v1/enrollment/invitations/other"], "the prompt path re-reads the invitation")
    }

    @Test func claimDigestChangedDuringPromptCancelsWithoutApproving() async {
        let secure = OwnerFaultyStore(); let producer = OwnerProducerFake(); let transport = OwnerTransportFake()
        let result = await host(producer, transport, secure).approve(prompt: {
            producer.value = .init(invitationId: "invite", stage: .verify, expiresInSeconds: 30, sas: "123456", claimDigest: "changed")
            return "vault secret"
        })
        #expect(result == .cancelled)
        #expect(secure.stored(account) == nil)
        #expect(producer.approvedSecrets.isEmpty)
        #expect(transport.requests == ["GET \(invitePath)", "GET \(invitePath)"])
    }

    @Test func displayedClaimDigestCannotBeReplacedBeforeApproval() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake()
        let approver = host(producer, transport, secure)
        _ = await approver.poll()
        producer.value = .init(invitationId: "invite", stage: .verify, expiresInSeconds: 30, sas: "654321", claimDigest: "other")

        #expect(await approver.approve(prompt: { nil }) == .cancelled)
        #expect(producer.approvedSecrets.isEmpty)
        #expect(transport.requests == ["GET \(invitePath)", "GET \(invitePath)"])
    }

    @Test func pollClaimReplacementCancelsWithoutPosting() async {
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake(); let approver = host(producer, transport)
        _ = await approver.poll()
        producer.value = .init(invitationId: "invite", stage: .verify, expiresInSeconds: 30, sas: "654321", claimDigest: "other")
        #expect(await approver.poll() == .cancelled)
        #expect(!transport.requests.contains { $0.hasPrefix("POST") })
    }

    @Test func repollingThePinnedClaimStillApproves() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake(); let approver = host(producer, transport, secure)
        _ = await approver.poll()
        _ = await approver.poll()
        #expect(await approver.approve(prompt: { nil }) == .view(approved(producer), qr: nil))
        #expect(transport.requests.contains("POST \(approvePath)"))
    }

    @Test func failingWriteStillApprovesAndSavesNothing() async {
        let secure = OwnerFaultyStore(); secure.failWrites = true
        let producer = OwnerProducerFake(); let transport = OwnerTransportFake(); let prompt = PromptScript(["vault secret"])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .view(approved(producer), qr: nil))
        #expect(secure.stored(account) == nil)
        #expect(producer.approvedSecrets == ["vault secret"])
        #expect(transport.requests == ["GET \(invitePath)", "GET \(invitePath)", "POST \(approvePath)"])
    }

    @Test func sharedSecretMethodNeverReadsOrPrompts() async {
        let secure = OwnerFaultyStore(); secure.failReads = true
        let producer = OwnerProducerFake(method: .sharedSecret); let transport = OwnerTransportFake(); let prompt = PromptScript(["unused"])
        #expect(await host(producer, transport, secure).approve(prompt: { await prompt.ask() }) == .view(approved(producer), qr: nil))
        #expect(secure.reads == 0)
        #expect(prompt.asked == 0)
        #expect(producer.approvedSecrets == [""])
    }

    @Test func sharedSecretClaimReplacementCancelsWithoutApproving() async {
        let producer = OwnerProducerFake(method: .sharedSecret); let transport = OwnerTransportFake()
        let approver = host(producer, transport)
        _ = await approver.poll()
        producer.value = .init(invitationId: "invite", stage: .verify, expiresInSeconds: 30, sas: "654321", claimDigest: "other")
        #expect(await approver.approve(prompt: { nil }) == .cancelled)
        #expect(producer.approvedSecrets.isEmpty)
    }

    @Test func approvalRequiresVerifyStage() async {
        let producer = OwnerProducerFake(stage: .waiting); let transport = OwnerTransportFake()
        #expect(await host(producer, transport).approve(prompt: { nil }) == .failed("enrollment-state"))
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    // MARK: Approval result

    @Test func approveReportsTheApprovedStageFromTheServerResponse() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
        let producer = OwnerProducerFake(); producer.statusByBody["approved"] = .init(invitationId: "invite", stage: .approved, expiresInSeconds: 30, sas: nil, claimDigest: "digest")
        let transport = OwnerTransportFake([http(200), .init(status: 200, body: "approved")])
        let result = await host(producer, transport, secure).approve(prompt: { nil })
        #expect(result == .view(producer.statusByBody["approved"]!, qr: nil))
    }

    @Test func approvedInvitationNeverRegressesToVerifyOrExpiredOrOffersRotation() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
        let producer = OwnerProducerFake(); producer.statusByBody["approved"] = .init(invitationId: "invite", stage: .approved, expiresInSeconds: 30, sas: nil, claimDigest: "digest")
        let transport = OwnerTransportFake([http(200), .init(status: 200, body: "approved")])
        let approver = host(producer, transport, secure)
        _ = await approver.approve(prompt: { nil })

        producer.value = .init(invitationId: "invite", stage: .expired, expiresInSeconds: 0, sas: nil, claimDigest: nil)
        let polled = await approver.poll()
        guard case let .view(status, _) = polled else { Issue.record("expected a view"); return }
        #expect(status.stage == .approved, "an approved invitation past expiry is still approved")
        #expect(await approver.rotateIfNeeded(requested: .transfer) == polled)
        #expect(await approver.cancel() == .failed("enrollment-state"))
        #expect(transport.requests.allSatisfy { !$0.hasPrefix("DELETE") && !$0.hasPrefix("POST /v1/enrollment/invitations ") })
    }

    @Test func approvedPollFailureNeverCreatesAReplacementInvitation() async {
        let secure = OwnerFaultyStore(); secure.seed(account, "synthetic vault secret")
        let producer = OwnerProducerFake()
        producer.statusByBody["approved"] = .init(invitationId: "invite", stage: .approved, expiresInSeconds: 30, sas: nil, claimDigest: "digest")
        let transport = OwnerTransportFake([http(200), .init(status: 200, body: "approved"), nil])
        let approver = host(producer, transport, secure)

        _ = await approver.approve(prompt: { nil })
        #expect(await approver.poll() == .failed("offline"))
        #expect(!transport.requests.contains("POST /v1/enrollment/invitations"))
    }

    // MARK: Rotation and cancellation

    @Test func waitingWithTimeLeftIsNotRotated() async {
        let transport = OwnerTransportFake()
        let result = await host(OwnerProducerFake(stage: .waiting, expiresIn: 30), transport).rotateIfNeeded(requested: .transfer)
        #expect(result == .view(OwnerProducerFake(stage: .waiting, expiresIn: 30).value, qr: "qr"))
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func nearExpiryWaitingRotatesWithDeleteThenPost() async {
        let transport = OwnerTransportFake()
        let result = await host(OwnerProducerFake(stage: .waiting, expiresIn: 15), transport).rotateIfNeeded(requested: .transfer)
        #expect(result == .view(.init(invitationId: "invite", stage: .waiting, expiresInSeconds: 0, sas: nil, claimDigest: nil), qr: "qr"))
        #expect(transport.requests == ["GET \(invitePath)", "DELETE \(invitePath)", "POST /v1/enrollment/invitations"])
    }

    @Test func expiredRotatesWithDeleteThenPost() async {
        let transport = OwnerTransportFake()
        _ = await host(OwnerProducerFake(stage: .expired, expiresIn: 30), transport).rotateIfNeeded(requested: .transfer)
        #expect(transport.requests == ["GET \(invitePath)", "DELETE \(invitePath)", "POST /v1/enrollment/invitations"])
    }

    @Test func methodChangeWhileWaitingRotates() async {
        let transport = OwnerTransportFake()
        _ = await host(OwnerProducerFake(stage: .waiting, expiresIn: 30, method: .transfer), transport).rotateIfNeeded(requested: .sharedSecret)
        #expect(transport.requests == ["GET \(invitePath)", "DELETE \(invitePath)", "POST /v1/enrollment/invitations"])
    }

    @Test func verifyNeverRotates() async {
        let transport = OwnerTransportFake()
        _ = await host(OwnerProducerFake(stage: .verify, expiresIn: 5), transport).rotateIfNeeded(requested: .transfer)
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func cancelNeverDeletesAVerifyInvitation() async {
        let transport = OwnerTransportFake()
        #expect(await host(OwnerProducerFake(stage: .verify), transport).cancel() == .failed("enrollment-state"))
        #expect(transport.requests == ["GET \(invitePath)"])
    }

    @Test func cancelDeletesWaitingInvitation() async {
        let transport = OwnerTransportFake()
        #expect(await host(OwnerProducerFake(stage: .waiting), transport).cancel() == .cancelled)
        #expect(transport.requests == ["GET \(invitePath)", "DELETE \(invitePath)"])
    }

    @Test func approvedAndConnectedStopPollingAndRotation() async {
        for stage in [OwnerPhoneStage.approved, .connected] {
            let transport = OwnerTransportFake()
            let approver = host(OwnerProducerFake(stage: stage, expiresIn: 3), transport)
            #expect(await approver.poll() == .view(OwnerProducerFake(stage: stage, expiresIn: 3).value, qr: nil))
            #expect(await approver.rotateIfNeeded(requested: .transfer) == .view(OwnerProducerFake(stage: stage, expiresIn: 3).value, qr: nil))
            #expect(transport.requests == ["GET \(invitePath)", "GET \(invitePath)"], "no DELETE or POST after approval")
        }
    }

    // MARK: Hosted sessions

    @Test func hostedFlowsFailBeforeAnyRequestWithoutAccountSession() async {
        let transport = OwnerTransportFake(); let approver = host(OwnerProducerFake(), transport, isHosted: true)
        #expect(await approver.poll() == .failed("account-required"))
        #expect(await approver.approve(prompt: { "vault secret" }) == .failed("account-required"))
        #expect(await approver.cancel() == .failed("account-required"))
        #expect(await approver.rotateIfNeeded(requested: .transfer) == .failed("account-required"))
        #expect(transport.requests.isEmpty)
    }

    @Test func hostedRequestsCarryTheAccountSession() async {
        let transport = OwnerTransportFake()
        _ = await host(OwnerProducerFake(stage: .waiting), transport, isHosted: true, session: "session").poll()
        #expect(transport.sessions == ["session"])
    }

    // MARK: HTTP and transport mapping

    @Test func pollMapsTransportAndHTTPFailures() async {
        let cases: [(OwnerPhoneHTTPResponse?, String)] = [
            (nil, "offline"), (http(404), "expired"), (http(410), "expired"), (http(409), "enrollment-state"),
            (http(401), "server-rejected"), (http(403), "server-rejected"), (http(500), "server-rejected"),
        ]
        for (response, code) in cases {
            #expect(await host(OwnerProducerFake(), OwnerTransportFake([response])).poll() == .failed(code))
        }
    }

    @Test func cancelMapsGoneInvitationsToSuccess() async {
        let cases: [(OwnerPhoneHTTPResponse?, OwnerPhoneResult)] = [
            (http(404), .cancelled), (http(410), .cancelled), (http(409), .failed("enrollment-state")),
            (http(500), .failed("server-rejected")), (nil, .failed("offline")),
        ]
        for (response, expected) in cases {
            let transport = OwnerTransportFake([http(200), response])
            #expect(await host(OwnerProducerFake(stage: .waiting), transport).cancel() == expected)
        }
    }

    @Test func approveMapsHTTPFailures() async {
        let cases: [(OwnerPhoneHTTPResponse?, String)] = [
            (nil, "offline"), (http(404), "expired"), (http(409), "enrollment-state"), (http(500), "server-rejected"),
        ]
        for (response, code) in cases {
            let secure = OwnerFaultyStore(); secure.seed(account, "vault secret")
            let transport = OwnerTransportFake([http(200), response])
            #expect(await host(OwnerProducerFake(), transport, secure).approve(prompt: { nil }) == .failed(code))
        }
    }

    @Test func startMapsOfflineTransport() async {
        #expect(await host(OwnerProducerFake(), OwnerTransportFake([nil])).start(method: .transfer) == .failed("offline"))
    }

    @Test func startWithoutAccountSessionSendsNoRequest() async {
        let transport = OwnerTransportFake()
        #expect(await host(OwnerProducerFake(), transport, isHosted: true).start(method: .transfer) == .failed("account-required"))
        #expect(transport.requests.isEmpty)
    }

    private func http(_ status: Int) -> OwnerPhoneHTTPResponse { .init(status: status, body: "{}") }

    /// The stage an approval success reports: the server's approved view of the same invitation.
    private func approved(_ producer: OwnerProducerFake) -> OwnerPhoneStatus {
        .init(invitationId: producer.value.invitationId, stage: .approved, expiresInSeconds: producer.value.expiresInSeconds, sas: nil, claimDigest: producer.value.claimDigest)
    }
}

private final class OwnerProducerFake: OwnerPhoneProducer, @unchecked Sendable {
    var value: OwnerPhoneStatus
    /// Statuses returned for specific response bodies; any other body reports `value`.
    var statusByBody: [String: OwnerPhoneStatus] = [:]
    var matches: Bool
    var currentMethod: OwnerPhoneMethod
    private(set) var approvedSecrets: [String] = []

    init(stage: OwnerPhoneStage = .verify, expiresIn: Int = 30, matches: Bool = true, method: OwnerPhoneMethod = .transfer) {
        value = .init(invitationId: "invite", stage: stage, expiresInSeconds: expiresIn, sas: stage == .verify ? "123456" : nil, claimDigest: "digest")
        self.matches = matches
        currentMethod = method
    }

    func createInvitationRequest(method: OwnerPhoneMethod) throws -> String { "{}" }
    func consumeCreated(_ response: String) throws {}
    func invitationId() throws -> String { value.invitationId }
    func qrPayload() throws -> String { "qr" }
    func status(_ response: String) throws -> OwnerPhoneStatus { statusByBody[response] ?? value }
    func method() throws -> OwnerPhoneMethod? { currentMethod }
    func secretMatches(_ secret: String) throws -> Bool { matches }
    func approveRequest(status: String, secret: String) throws -> String { approvedSecrets.append(secret); return "{}" }
}

private final class OwnerTransportFake: OwnerPhoneTransport, @unchecked Sendable {
    private var queue: [OwnerPhoneHTTPResponse?]
    private(set) var requests: [String] = []
    private(set) var sessions: [String?] = []

    /// Responses are consumed in order; once drained every request answers 200.
    init(_ queue: [OwnerPhoneHTTPResponse?] = []) { self.queue = queue }

    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        requests.append("\(method) \(path)")
        sessions.append(accountSession)
        return queue.isEmpty ? .init(status: 200, body: "{}") : queue.removeFirst()
    }
}

private final class PromptScript: @unchecked Sendable {
    private var answers: [String?]
    private(set) var asked = 0

    init(_ answers: [String?]) { self.answers = answers }

    func ask() async -> String? {
        asked += 1
        return answers.isEmpty ? nil : answers.removeFirst()
    }
}

private final class OwnerFaultyStore: SecureStore, @unchecked Sendable {
    private let inner = InMemorySecureStore()
    var failReads = false
    var failWrites = false
    private(set) var reads = 0

    func seed(_ account: String, _ value: String) { inner.seed(account, value) }
    func stored(_ account: String) -> String? { inner.value(account).flatMap { String(data: $0, encoding: .utf8) } }

    func read(_ account: String) throws(ClientError) -> Data? {
        reads += 1
        if failReads { throw .unexpected("keychain read") }
        return inner.value(account)
    }
    func addIfAbsent(_ account: String, _ data: Data) throws(ClientError) -> Bool {
        if failWrites { throw .unexpected("keychain write") }
        return try inner.addIfAbsent(account, data)
    }
    func upsert(_ account: String, _ data: Data) throws(ClientError) {
        if failWrites { throw .unexpected("keychain write") }
        try inner.upsert(account, data)
    }
    func delete(_ account: String) throws(ClientError) { try inner.delete(account) }
}

private extension InMemorySecureStore {
    func seed(_ account: String, _ value: String) { try? upsert(account, Data(value.utf8)) }
}
