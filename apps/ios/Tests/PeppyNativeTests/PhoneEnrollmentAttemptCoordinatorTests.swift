import Testing
@testable import PeppyNative

@MainActor
@Suite struct PhoneEnrollmentAttemptCoordinatorTests {
    @Test func approvedTransferConfirmsOnlyOnceAndCompletesOnce() {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        let attempt = coordinator.beginScan()!

        #expect(coordinator.received(PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false), for: attempt) == .confirm(attempt))
        #expect(coordinator.received(PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false), for: attempt) == .none)
        let completed = coordinator.finishedConfirmation(for: attempt, outcome: .succeeded)
        let lateCompletion = coordinator.finishedConfirmation(for: attempt, outcome: .succeeded)
        #expect(completed)
        #expect(!lateCompletion)
    }

    @Test func sharedSecretEnrollmentNeverConfirmsAutomatically() {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        let attempt = coordinator.beginScan()!

        #expect(coordinator.received(PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: true), for: attempt) == .none)
        #expect(coordinator.beginManualConfirmation() == attempt)
    }

    @Test func failedConfirmationRetriesTheCurrentAttemptOnlyWhenRequested() {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        let attempt = coordinator.beginScan()!
        _ = coordinator.received(PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false), for: attempt)

        let failed = coordinator.finishedConfirmation(for: attempt, outcome: .retryableFailure)
        #expect(!failed)
        #expect(coordinator.retryConfirmation() == attempt)
    }

    @Test func canceledAttemptCannotCompleteAfterItsScanIsSuperseded() {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        let first = coordinator.beginScan()!
        _ = coordinator.received(PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false), for: first)
        coordinator.cancel()
        let second = coordinator.beginScan()!

        let staleCompletion = coordinator.finishedConfirmation(for: first, outcome: .succeeded)
        #expect(!staleCompletion)
        #expect(second != first)
    }

    @Test func runnerConfirmsApprovedStatusAfterPendingPollWithoutCancelingItsAttempt() async {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        _ = coordinator.beginScan()
        var updates: [PhoneEnrollmentStatus] = []
        let outcome = await coordinator.run(
            initial: PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: true, requiresSharedSecret: false),
            poll: { PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false) },
            wait: {},
            onStatus: { updates.append($0) },
            confirm: {
                await Task.yield()
                return .succeeded
            }
        )

        #expect(outcome == .succeeded)
        #expect(updates.count == 2)
    }

    @Test func runnerCancellationCannotCompleteOrMutateASupersedingAttempt() async {
        let coordinator = PhoneEnrollmentAttemptCoordinator()
        _ = coordinator.beginScan()
        let gate = ConfirmationGate()
        let task = Task { @MainActor in
            await coordinator.run(
                initial: PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: true, requiresSharedSecret: false),
                poll: { PhoneEnrollmentStatus(verificationCode: "123456", awaitingOwner: false, requiresSharedSecret: false) },
                wait: {},
                onStatus: { _ in },
                confirm: {
                    await gate.enter()
                    return .succeeded
                }
            )
        }

        await gate.waitUntilEntered()
        #expect(coordinator.isConfirming)
        coordinator.cancel()
        let replacement = coordinator.beginScan()
        await gate.release()

        #expect(await task.value == .canceled)
        #expect(coordinator.activeAttempt == replacement)
        #expect(!coordinator.isConfirming)
    }
}

private actor ConfirmationGate {
    private var entered = false
    private var enteredWaiter: CheckedContinuation<Void, Never>?
    private var releaseWaiter: CheckedContinuation<Void, Never>?

    func enter() async {
        entered = true
        enteredWaiter?.resume()
        await withCheckedContinuation { releaseWaiter = $0 }
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { enteredWaiter = $0 }
    }

    func release() {
        releaseWaiter?.resume()
        releaseWaiter = nil
    }
}
