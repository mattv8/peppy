import Foundation

/// Small, non-secret host scheduling state. Snapshot progress itself remains in the Rust core.
public final class SnapshotTooLargeStore: @unchecked Sendable {
    /// Where the too-large rejection came from. A begin is refused before any download (record ceiling).
    /// An append is refused after the device has downloaded up to the byte cap.
    public enum Rejection: String, Codable, Equatable, Sendable {
        case begin
        case append
    }

    public struct Marker: Codable, Equatable, Sendable {
        public let checkedAt: Date
        public let build: String
        /// Nil in markers written before the kind was recorded; those are begin rejections.
        public let rejection: Rejection?

        public init(checkedAt: Date, build: String, rejection: Rejection?) {
            self.checkedAt = checkedAt
            self.build = build
            self.rejection = rejection
        }

        public var rejectionKind: Rejection { rejection ?? .begin }
    }

    /// Recheck interval after a begin rejection: no download was spent, so retrying is cheap.
    public static let beginRecheckInterval: TimeInterval = 6 * 60 * 60
    /// Recheck interval after an append rejection: the device already downloaded up to the byte cap,
    /// so retrying four times a day would repeat hundreds of megabytes.
    public static let appendRecheckInterval: TimeInterval = 7 * 24 * 60 * 60

    private let defaults: UserDefaults
    private let key = "peppy.snapshot-too-large"
    private let lock = NSLock()

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func marker(for vaultID: String) -> Marker? {
        lock.withLock {
            let markers = defaults.data(forKey: key).flatMap { try? JSONDecoder().decode([String: Marker].self, from: $0) } ?? [:]
            return markers[vaultID]
        }
    }

    /// A marker blocks a new cut for the same build until its recheck interval has passed. A build change
    /// allows one attempt at once, and a wall-clock step backwards (`now` before `checkedAt`) counts as expired.
    public func shouldBeginSnapshot(vaultID: String, build: String, now: Date = Date()) -> Bool {
        guard let marker = marker(for: vaultID) else { return true }
        if marker.build != build || now < marker.checkedAt { return true }
        let interval = marker.rejectionKind == .append ? Self.appendRecheckInterval : Self.beginRecheckInterval
        return now.timeIntervalSince(marker.checkedAt) >= interval
    }

    public func mark(vaultID: String, build: String, rejection: Rejection = .begin, now: Date = Date()) {
        update { $0[vaultID] = Marker(checkedAt: now, build: build, rejection: rejection) }
    }

    public func clear(vaultID: String) {
        update { $0.removeValue(forKey: vaultID) }
    }

    private func update(_ body: (inout [String: Marker]) -> Void) {
        lock.withLock {
            var markers = defaults.data(forKey: key).flatMap { try? JSONDecoder().decode([String: Marker].self, from: $0) } ?? [:]
            body(&markers)
            defaults.set(try? JSONEncoder().encode(markers), forKey: key)
        }
    }
}
