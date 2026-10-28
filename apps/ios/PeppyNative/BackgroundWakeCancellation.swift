import Foundation

/// Main-actor state shared by the APNs expiry callback and its background task. Installing after
/// expiry immediately cancels the new task, closing the launch/expiry race without shared mutable
/// state in UIKit's sendable expiration closure.
@MainActor
public final class BackgroundWakeCancellation {
    private var expired = false
    private var task: Task<Void, Never>?

    public init() {}

    public func install(_ task: Task<Void, Never>) {
        self.task = task
        if expired { task.cancel() }
    }

    public func expire() {
        expired = true
        task?.cancel()
    }
}

/// Main-actor, one-shot completion/end lifecycle shared by APNs expiry and normal completion.
/// Expiry calls this synchronously so a blocked FFI task cannot retain UIKit background time.
@MainActor
public final class BackgroundWakeLifecycle {
    private var ended = false
    // Main-actor closures, not `@Sendable`: the app's `end` owns the UIKit background-task
    // identifier as a captured main-actor variable.
    private let complete: @MainActor (Bool) -> Void
    private let end: @MainActor () -> Void

    public init(complete: @escaping @MainActor (Bool) -> Void, end: @escaping @MainActor () -> Void) {
        self.complete = complete
        self.end = end
    }

    public func finish(_ success: Bool) {
        guard !ended else { return }
        ended = true
        complete(success)
        end()
    }

    public func expire(_ cancellation: BackgroundWakeCancellation) {
        cancellation.expire()
        finish(false)
    }
}
