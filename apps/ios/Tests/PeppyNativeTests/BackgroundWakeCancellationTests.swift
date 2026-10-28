import Testing
@testable import PeppyNative

@Suite struct BackgroundWakeCancellationTests {
    @Test func expiryBeforeInstallCancelsTheNewTask() async {
        let holder = await MainActor.run { BackgroundWakeCancellation() }
        await holder.expire()
        let task = Task<Void, Never> { try? await Task.sleep(for: .seconds(60)) }
        await holder.install(task)
        #expect(task.isCancelled)
    }

    @Test func expiryAndCompletionEndOnce() async {
        let cancellation = await MainActor.run { BackgroundWakeCancellation() }
        let calls = Locked((completion: 0, ended: 0, result: true))
        let lifecycle = await MainActor.run {
            BackgroundWakeLifecycle(
                complete: { success in calls.withValue { $0.completion += 1; $0.result = success } },
                end: { calls.withValue { $0.ended += 1 } }
            )
        }
        await MainActor.run { lifecycle.expire(cancellation); lifecycle.finish(true) }
        #expect(calls.value.completion == 1)
        #expect(calls.value.ended == 1)
        #expect(!calls.value.result)
    }

    /// Mirrors PushWakeDelegate: `end` owns a captured main-actor identifier, which only compiles
    /// under Swift 6 because the lifecycle closures are main-actor isolated rather than `@Sendable`.
    @Test func mainActorEndOwnsItsCapturedIdentifierAndEndsOnce() async {
        let ended = await MainActor.run { () -> [Int] in
            let cancellation = BackgroundWakeCancellation()
            var identifier = 7
            var ended: [Int] = []
            let lifecycle = BackgroundWakeLifecycle(
                complete: { _ in },
                end: {
                    guard identifier != 0 else { return }
                    ended.append(identifier)
                    identifier = 0
                }
            )
            lifecycle.expire(cancellation)
            lifecycle.finish(true)
            return ended
        }
        #expect(ended == [7])
    }
}
