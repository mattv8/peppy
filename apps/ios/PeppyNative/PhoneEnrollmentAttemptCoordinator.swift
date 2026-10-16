import Foundation
import Observation

/// Keeps one phone-enrollment scan and confirmation attempt active at a time.
@MainActor @Observable
public final class PhoneEnrollmentAttemptCoordinator {
    public enum Action: Equatable, Sendable {
        case none
        case confirm(UUID)
    }

    public enum ConfirmationOutcome: Equatable, Sendable {
        case succeeded
        case canceled
        case retryableFailure
        case terminalFailure
    }

    private enum Phase {
        case idle, scanning, polling, confirming, retryableFailure, terminalFailure, completed
    }

    private var attempt: UUID?
    private var phase = Phase.idle

    public init() {}

    public func beginScan() -> UUID? {
        guard phase == .idle else { return nil }
        let next = UUID()
        attempt = next
        phase = .scanning
        return next
    }

    public func received(_ status: PhoneEnrollmentStatus, for attempt: UUID) -> Action {
        guard self.attempt == attempt, phase != .confirming, phase != .retryableFailure, phase != .terminalFailure, phase != .completed else { return .none }
        guard !status.awaitingOwner else {
            phase = .polling
            return .none
        }
        guard !status.requiresSharedSecret else {
            phase = .polling
            return .none
        }
        phase = .confirming
        return .confirm(attempt)
    }

    public func beginManualConfirmation() -> UUID? {
        guard let attempt, phase == .polling else { return nil }
        phase = .confirming
        return attempt
    }

    public func finishedConfirmation(for attempt: UUID, outcome: ConfirmationOutcome) -> Bool {
        guard self.attempt == attempt, phase == .confirming else { return false }
        switch outcome {
        case .succeeded: phase = .completed; return true
        case .canceled: phase = .idle; self.attempt = nil
        case .retryableFailure: phase = .retryableFailure
        case .terminalFailure: phase = .terminalFailure
        }
        return false
    }

    public func retryConfirmation() -> UUID? {
        guard let attempt, phase == .retryableFailure else { return nil }
        phase = .confirming
        return attempt
    }

    public func cancel() {
        attempt = nil
        phase = .idle
    }

    public var isConfirming: Bool { phase == .confirming }
    public var isStarting: Bool { phase == .scanning }
    public var canRetryConfirmation: Bool { phase == .retryableFailure }
    public var needsRescan: Bool { phase == .terminalFailure }
    public var activeAttempt: UUID? { attempt }

    /// Runs the status-to-confirm transition for one owned UI operation. Callers provide the
    /// transport operations so lifecycle behavior is testable without SwiftUI task identity.
    public func run(
        initial: PhoneEnrollmentStatus,
        poll: () async -> PhoneEnrollmentStatus?,
        wait: () async -> Void,
        onStatus: (PhoneEnrollmentStatus) -> Void,
        confirm: () async -> ConfirmationOutcome
    ) async -> ConfirmationOutcome? {
        guard let attempt else { return .canceled }
        var status = initial
        while !Task.isCancelled, self.attempt == attempt {
            let action = received(status, for: attempt)
            onStatus(status)
            if case .confirm = action {
                let outcome = await confirm()
                guard !Task.isCancelled,
                      finishedConfirmation(for: attempt, outcome: outcome) else {
                    return .canceled
                }
                return outcome
            }
            guard status.awaitingOwner else { return nil }
            await wait()
            guard !Task.isCancelled, self.attempt == attempt,
                  let next = await poll(),
                  !Task.isCancelled, self.attempt == attempt else { return .canceled }
            status = next
        }
        return .canceled
    }
}

/// Rejects a quiet owner-phone poll result when a foreground action began after it.
@MainActor
public final class OwnerPhonePollingCoordinator {
    private var generation = 0
    private var activeAction: Int?

    public init() {}

    public func beginPoll() -> Int { generation }

    public func shouldApplyPoll(_ pollGeneration: Int) -> Bool {
        activeAction == nil && generation == pollGeneration
    }

    public func beginAction() -> Int {
        generation &+= 1
        activeAction = generation
        return generation
    }

    public func finishAction(_ actionGeneration: Int) {
        guard activeAction == actionGeneration else { return }
        activeAction = nil
    }
}
