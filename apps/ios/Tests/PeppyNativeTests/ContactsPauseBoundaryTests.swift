import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

private actor PauseGate {
    private var entered = false
    private var released = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func waitUntilEntered() async {
        if entered { return }
        await withCheckedContinuation { entryWaiters.append($0) }
    }

    func wait() async {
        entered = true
        entryWaiters.forEach { $0.resume() }
        entryWaiters.removeAll()
        if released { return }
        await withCheckedContinuation { releaseWaiters.append($0) }
    }

    func release() {
        released = true
        releaseWaiters.forEach { $0.resume() }
        releaseWaiters.removeAll()
    }
}

private actor PausedProvider: ContactsProvider {
    enum Boundary: Equatable, Sendable { case access, containers, changes, photo, save }

    let base: WritableFakeProvider
    let boundary: Boundary
    let gate: PauseGate

    init(base: WritableFakeProvider, boundary: Boundary, gate: PauseGate) {
        self.base = base
        self.boundary = boundary
        self.gate = gate
    }

    private func pause(at boundary: Boundary) async {
        if self.boundary == boundary { await gate.wait() }
    }

    func access() async throws -> ContactsAccess {
        await pause(at: .access)
        return try await base.access()
    }

    func containers() async throws -> [ContactContainer] {
        await pause(at: .containers)
        return try await base.containers()
    }

    func fetchAll(isCancelled: @Sendable () -> Bool) async throws -> ContactsFetch {
        try await base.fetchAll(isCancelled: isCancelled)
    }

    func fetchPage(after: String?, limit: Int, isCancelled: @Sendable () -> Bool) async throws -> ContactsPage {
        try await base.fetchPage(after: after, limit: limit, isCancelled: isCancelled)
    }

    func contact(id: String) async throws -> PlatformContact? {
        try await base.contact(id: id)
    }

    func historyToken() async -> Data? { await base.historyToken() }

    func changes(since token: Data, limit: Int) async -> ContactsHistoryChanges {
        await pause(at: .changes)
        return await base.changes(since: token, limit: limit)
    }

    func photo(id: String, thumbnail: Bool) async throws -> ContactPhotoRead {
        await pause(at: .photo)
        return try await base.photo(id: id, thumbnail: thumbnail)
    }

    func save(_ write: ContactsWrite) async throws -> ContactsWriteResult {
        await pause(at: .save)
        return try await base.save(write)
    }

    func identifiers(matchingName name: String) async throws -> [String]? {
        try await base.identifiers(matchingName: name)
    }
}

@Suite(.serialized) struct ContactsPauseBoundaryTests {
    private let passphrase = "io synthetic vault passphrase"

    private func coordinator(
        _ provider: any ContactsProvider,
        preferences: InMemoryContactsPreferences
    ) throws -> (NativeContactsCoordinator, NativeClient) {
        let server = try FakeServer(passphrase: passphrase)
        let deviceId = UUID().uuidString.lowercased()
        let client = try openUnlockedClient(server, passphrase: passphrase, deviceId: deviceId)
        try client.registerDeviceHint(platform: "ios", hint: Data(repeating: 8, count: 32))
        try client.observeServerHighWater(highWaterCursor: "0")
        return (NativeContactsCoordinator(client: client, deviceId: deviceId, provider: provider, preferences: preferences), client)
    }

    private func bookView(_ client: NativeClient, bookId: String) throws -> String {
        try client.contactBookView(inputJson: NativeContactsCoordinator.json([
            "book_id": bookId, "limit": 200,
        ]))
    }

    private func pauseDuring(
        _ boundary: PausedProvider.Boundary,
        configure: @escaping @Sendable (WritableFakeProvider) async -> Void = { _ in }
    ) async throws -> (ContactsSyncReport, String, String, WritableFakeProvider, InMemoryContactsPreferences, NativeContactsCoordinator) {
        let base = WritableFakeProvider()
        await base.configure { $0.book = sampleContacts(1); $0.historyEnabled = true }
        let preferences = InMemoryContactsPreferences(ContactsPreferences(enabled: true))
        let initial = try coordinator(base, preferences: preferences)
        let first = try await initial.0.run(reason: .foreground, isCancelled: { false })
        let bookId = try #require(first.bookId)
        let before = try bookView(initial.1, bookId: bookId)
        await configure(base)

        let gate = PauseGate()
        let paused = PausedProvider(base: base, boundary: boundary, gate: gate)
        let subject = NativeContactsCoordinator(client: initial.1, deviceId: initial.0.deviceId, provider: paused, preferences: preferences)
        async let pass = subject.run(reason: .foreground, isCancelled: { !preferences.load().enabled })
        await gate.waitUntilEntered()
        preferences.save(ContactsPreferences(intent: .stopped))
        await gate.release()
        let report = try await pass
        return (report, before, try bookView(initial.1, bookId: bookId), base, preferences, initial.0)
    }

    @Test(arguments: [PausedProvider.Boundary.access, .containers, .changes])
    private func pausingAfterReadKeepsPublishedBookAndCheckpoint(boundary: PausedProvider.Boundary) async throws {
        let (report, before, after, _, preferences, _) = try await pauseDuring(boundary) { base in
            if boundary == .changes { await base.rename("os-c0", given: "Changed while paused") }
        }
        #expect(report.outcome == .partial)
        #expect(preferences.load().intent == .stopped)
        // A stopped pass is not allowed to publish a fresh capture after its suspended OS read.
        #expect(after == before)
    }

    @Test func pausingDuringPhotoReadDoesNotPrepareOrCaptureThePhoto() async throws {
        let image = try #require(syntheticImage())
        let (report, before, after, base, preferences, resumed) = try await pauseDuring(.photo) { base in
            await base.setPhoto("os-c0", image)
        }
        #expect(report.photosCaptured == 0 && report.photosRemoved == 0)
        #expect(!before.contains("attachment_id"))
        #expect(after == before)
        #expect(await base.photoReads == 1)
        preferences.save(ContactsPreferences(intent: .on))
        let captured = try await resumed.run(reason: .foreground, isCancelled: { false })
        let settled = try await resumed.run(reason: .foreground, isCancelled: { false })
        #expect(captured.photosCaptured == 1)
        #expect(settled.photosCaptured == 0)
    }

    @Test func pauseAfterAnIssuedSaveReconcilesItsReturnedEvidenceWithoutRetrying() async throws {
        let base = WritableFakeProvider()
        await base.configure { $0.book = sampleContacts(1) }
        let preferences = InMemoryContactsPreferences(ContactsPreferences(enabled: true))
        let ready = try coordinator(base, preferences: preferences)
        let first = try await ready.0.run(reason: .foreground, isCancelled: { false })
        let bookId = try #require(first.bookId)
        let gate = PauseGate()
        let paused = PausedProvider(base: base, boundary: .save, gate: gate)
        let subject = NativeContactsCoordinator(client: ready.1, deviceId: ready.0.deviceId, provider: paused, preferences: preferences)
        let request: [String: Any] = ["kind": "create", "name": ["given": "Saved"], "display_name": "Saved"]

        async let outcome = subject.apply(request: request, requestId: UUID().uuidString, book: [:], bookId: bookId, access: .full)
        await gate.waitUntilEntered()
        preferences.save(ContactsPreferences(intent: .stopped))
        await gate.release()

        guard case .applied = await outcome else { Issue.record("issued save must reconcile its returned OS evidence"); return }
        #expect(await base.saves.count == 1)
        #expect(await base.book.contains { $0.givenName == "Saved" })
    }

    @Test func pausedActiveSweepRetainsTheInterruptedPhotoForOneLaterCapture() async throws {
        let image = try #require(syntheticImage())
        let base = WritableFakeProvider()
        await base.configure { $0.book = sampleContacts(2); $0.historyEnabled = true }
        let preferences = InMemoryContactsPreferences(ContactsPreferences(enabled: true))
        let initial = try coordinator(base, preferences: preferences)
        let first = try await initial.0.run(reason: .foreground, isCancelled: { false })
        let bookId = try #require(first.bookId)
        var checkpoint = try initial.0.readCheckpoint(bookId)
        checkpoint.sweepActive = true
        checkpoint.sweepOffset = 0
        try initial.0.writeCheckpoint(bookId, checkpoint)
        // Deliberately leave history unchanged: only the active sweep can observe this photo.
        await base.configure { $0.photos["os-c0"] = image }

        let gate = PauseGate()
        let paused = PausedProvider(base: base, boundary: .photo, gate: gate)
        let subject = NativeContactsCoordinator(client: initial.1, deviceId: initial.0.deviceId, provider: paused, preferences: preferences)
        async let pass = subject.run(reason: .foreground, isCancelled: { !preferences.load().enabled })
        await gate.waitUntilEntered()
        preferences.save(ContactsPreferences(intent: .stopped))
        await gate.release()
        _ = try await pass

        let pausedCheckpoint = try initial.0.readCheckpoint(bookId)
        #expect(pausedCheckpoint.sweepOffset == 0)
        preferences.save(ContactsPreferences(intent: .on))
        let resumed = try await initial.0.run(reason: .foreground, isCancelled: { false })
        let settled = try await initial.0.run(reason: .foreground, isCancelled: { false })
        #expect(resumed.photosCaptured == 1)
        #expect(settled.photosCaptured == 0)
    }

    @Test func enabledControlCapturesAndUpdatesNormally() async throws {
        let base = WritableFakeProvider()
        await base.configure { $0.book = sampleContacts(1); $0.historyEnabled = true }
        let preferences = InMemoryContactsPreferences(ContactsPreferences(enabled: true))
        let subject = try coordinator(base, preferences: preferences)
        let first = try await subject.0.run(reason: .foreground, isCancelled: { false })
        await base.rename("os-c0", given: "Enabled update")
        let second = try await subject.0.run(reason: .foreground, isCancelled: { false })
        #expect(first.captured == 1)
        #expect(second.outcome == .incremental && second.captured == 1)
    }
}
