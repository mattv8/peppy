import Foundation
import Testing
@testable import PeppyNative

@Suite struct OwnerPhoneLostResponseTests {
    @Test func committedApprovalWithLostResponseReplaysOnRetry() async throws {
        let identity = EnrollmentIdentity(origin: "https://vault.example.test", vaultId: "vault", deviceId: "device")
        let producer = LostResponseProducer()
        let transport = LostResponseTransport()
        let secure = InMemorySecureStore()
        let host = OwnerComputerApprovalHost(
            producer: producer, transport: transport, secrets: VaultSecretStore(secure: secure),
            identity: identity, credential: "device"
        )

        // Review finding D-08 covers an approval whose response is lost after the server committed it.
        // Today the first attempt reports offline; an identical retry gets the server's replayed 200
        // and approves. The successful replay is correct and must keep working.
        // Pins current behaviour of review finding D-08; flip this assertion when the fix is authorized.
        #expect(await host.approve(codesMatch: true, prompt: { "vault phrase" }) == .failed("offline"))
        #expect(transport.committed)
        #expect(try producer.view() == producer.approval)
        #expect(secure.value(identity.account("vault-secret-v2")) == Data("vault phrase".utf8))

        #expect(await host.approve(codesMatch: true, prompt: { nil }) == .approved)
        #expect(transport.bodies == ["approval", "approval"])
        #expect(producer.phrases == ["vault phrase", "vault phrase"])
        #expect(try producer.view() == producer.approval)
        #expect(secure.value(identity.account("vault-secret-v2")) == Data("vault phrase".utf8))
        #expect(await host.cancel() == .cancelled)
        #expect(producer.cancellations == 0)
        #expect(transport.requests == [
            "POST /v1/enrollment/computers/invite/approve",
            "POST /v1/enrollment/computers/invite/approve",
        ])
    }
}

private final class LostResponseProducer: OwnerComputerProducer, @unchecked Sendable {
    let approval = OwnerComputerApproval(invitationID: "invite", promptDigest: "digest", sas: "123456")
    private(set) var phrases: [String] = []
    private(set) var cancellations = 0

    func createRequest(qr: String) throws -> OwnerComputerPrompt {
        .init(origin: "https://vault.example.test", invitationID: "invite", promptDigest: "digest", requestJSON: "request")
    }
    func consumeCreated(invitationID: String, promptDigest: String, response: String) throws {}
    func view() throws -> OwnerComputerApproval? { approval }
    func approve(invitationID: String, promptDigest: String, codesMatch: Bool, vaultKeyPhrase: String) throws -> String {
        phrases.append(vaultKeyPhrase)
        return "approval"
    }
    func cancel() throws { cancellations += 1 }
}

private final class LostResponseTransport: OwnerComputerTransport, @unchecked Sendable {
    private(set) var committed = false
    private(set) var requests: [String] = []
    private(set) var bodies: [String?] = []

    func request(method: String, path: String, body: String?, credential: String, accountSession: String?) async throws -> OwnerPhoneHTTPResponse? {
        requests.append("\(method) \(path)")
        bodies.append(body)
        if !committed {
            committed = true
            return nil
        }
        // The server returns the approved status for an identical replay.
        return .init(status: body == bodies.first! ? 200 : 409, body: "{}")
    }
}
