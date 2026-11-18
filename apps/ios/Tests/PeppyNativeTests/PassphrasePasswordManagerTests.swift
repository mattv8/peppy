import Testing
@testable import PeppyNative

@Suite @MainActor struct PassphrasePasswordManagerTests {
    @Test func saveOutcomesUpdateTheStatus() async {
        let state = PassphrasePasswordSaveState()

        await state.save(username: "Peppy Vault key phrase", phrase: "phrase", using: FakeSaver(.saved))
        #expect(state.status == .saved)

        state.reset()
        await state.save(username: "Peppy Vault key phrase", phrase: "phrase", using: FakeSaver(.cancelled))
        #expect(state.status == .idle)

        await state.save(username: "Peppy Vault key phrase", phrase: "phrase", using: FakeSaver(.failed))
        #expect(state.status == .failed)
    }

    @Test func resetIgnoresAStaleSaveResult() async {
        let state = PassphrasePasswordSaveState()
        let saver = DeferredSaver()

        let task = Task {
            await state.save(username: "Peppy Vault key phrase", phrase: "phrase", using: saver)
        }
        for await _ in saver.started { break }
        #expect(state.status == .saving)

        state.reset()
        saver.finish(with: .saved)
        await task.value

        #expect(state.status == .idle)
    }
}

@MainActor private struct FakeSaver: PassphrasePasswordSaving {
    let outcome: PassphrasePasswordSaveOutcome

    init(_ outcome: PassphrasePasswordSaveOutcome) {
        self.outcome = outcome
    }

    func save(username: String, phrase: String) async -> PassphrasePasswordSaveOutcome {
        outcome
    }
}

@MainActor private final class DeferredSaver: PassphrasePasswordSaving {
    let started: AsyncStream<Void>
    private let startedContinuation: AsyncStream<Void>.Continuation
    private var continuation: CheckedContinuation<PassphrasePasswordSaveOutcome, Never>?

    init() {
        (started, startedContinuation) = AsyncStream.makeStream()
    }

    func save(username: String, phrase: String) async -> PassphrasePasswordSaveOutcome {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            startedContinuation.yield()
        }
    }

    func finish(with outcome: PassphrasePasswordSaveOutcome) {
        continuation?.resume(returning: outcome)
        continuation = nil
    }
}
