import Foundation
import Testing
@testable import PeppyNative

@Suite(.serialized) struct ContactsLimitedAccessSettleTests {
    // Regression for review exception E5.
    @Test(arguments: [true, false])
    func interruptedDeleteRequiresFreshFullAccess(limited: Bool) async throws {
        let fixtures = ContactsIntegrationTests()
        let owner = try await fixtures.owner(sampleContacts(1))
        let bookId = try #require(try await owner.run().bookId)
        try owner.coordinator.setRemoteEdits("auto")
        let target = try #require(try owner.view().first)
        let requester = try fixtures.requester(owner)
        let delete = fixtures.request(owner, bookId: bookId, [
            "kind": "delete", "contact_id": target["id"]!, "base_revision": target["revision"]!,
        ])
        let requestId = try #require(delete["request_id"] as? String)
        _ = try requester.requestContactEdit(inputJson: NativeContactsCoordinator.json(delete))
        try await fixtures.deliver(from: requester, to: owner)
        try owner.coordinator.decide(requestId, approve: true)
        let permit = try NativeContactsCoordinator.object(owner.client.nextContactApplyPermit(
            inputJson: NativeContactsCoordinator.json(["request_id": requestId])
        ))
        #expect(permit["status"] as? String == "permit")

        let accessAtStart = try await owner.provider.access()
        #expect(accessAtStart == .full)
        await owner.provider.configure {
            if limited {
                $0.state = .limited
                $0.visible = []
            } else {
                $0.book = []
            }
        }
        var checkpoint = ContactsCheckpoint()
        var report = ContactsSyncReport(outcome: .unchanged)
        let book = owner.coordinator.bookBody(id: bookId, generation: "1", access: accessAtStart, containers: [])
        try await owner.coordinator.processRequests(
            book: book, bookId: bookId, access: accessAtStart, checkpoint: &checkpoint,
            report: &report, isCancelled: { false }
        )
        #expect(report.applied == (limited ? 0 : 1))
        #expect(report.outcomeUnknown == (limited ? 1 : 0))
        #expect(try fixtures.ledgerState(owner, requestId) == (limited ? "outcome_unknown" : "applied"))
        #expect(try owner.view().contains { $0["id"] as? String == target["id"] as? String } == limited)
    }
}
