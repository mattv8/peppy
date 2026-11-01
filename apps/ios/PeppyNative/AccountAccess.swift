import Foundation
import Observation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

/// Account write-access notice kinds, mirroring the shared Rust decision. Raw values are the
/// snake_case kind names used in copy keys (`peppy.account_<raw value>_title`).
public enum AccountNoticeKind: String, Codable, Equatable, Hashable, Sendable {
    case paymentFailedGrace = "payment_failed_grace"
    case endingScheduled = "ending_scheduled"
    case readOnlyPastDue = "read_only_past_due"
    case readOnlyCanceled = "read_only_canceled"
    case readOnlyIncomplete = "read_only_incomplete"
    case readOnlyNoSubscription = "read_only_no_subscription"
    case readOnlyTesterIneligible = "read_only_tester_ineligible"
    case readOnlyAccountDeleting = "read_only_account_deleting"
    case readOnlyOther = "read_only_other"
}

/// `error` means read-only (authoritative and never dismissible); `warning` is advisory.
public enum AccountNoticeSeverity: String, Codable, Equatable, Sendable {
    case warning
    case error
}

/// One notice as decided by Rust. Clients keep it and map it to copy; they never re-derive its rules.
public struct AccountNoticeSnapshot: Codable, Equatable, Sendable {
    public let kind: AccountNoticeKind
    public let severity: AccountNoticeSeverity
    /// Unix seconds, or nil when the notice has no deadline.
    public let deadline: Int64?
    /// A validated HTTPS account URL, or nil when the ribbon has no action.
    public let manageUrl: String?
    public let dismissible: Bool
    public let snoozeKey: String
}

/// What the global ribbon shows. Text is catalog keys plus formatted arguments; the view resolves them.
public struct SystemRibbonPresentation: Equatable, Sendable {
    public enum Source: Equatable, Sendable {
        case serverOffline
        case account(AccountNoticeKind)
    }

    public struct Action: Equatable, Sendable {
        public let labelKey: String
        public let url: URL
    }

    public let source: Source
    public let severity: AccountNoticeSeverity
    public let titleKey: String
    public let titleArgument: String?
    public let bodyKey: String
    public let bodyArgument: String?
    public let action: Action?
    /// Present only for a dismissible account notice; the key of its 24-hour snooze.
    public let snoozeKey: String?

    /// Stable identifier for the slot's kind, used for transition announcements.
    public var kindIdentifier: String {
        switch source {
        case .serverOffline: "server_offline"
        case .account(let kind): kind.rawValue
        }
    }
}

/// Picks the one ribbon for the slot: server offline, then the account notice.
public enum SystemRibbonResolver {
    /// - Parameter notice: a notice already filtered for identity and snooze.
    public static func presentation(serverOffline: Bool, notice: AccountNoticeSnapshot?, now: Date) -> SystemRibbonPresentation? {
        if serverOffline { return serverOfflinePresentation }
        guard let notice else { return nil }
        return accountPresentation(notice, now: now)
    }

    private static let serverOfflinePresentation = SystemRibbonPresentation(
        source: .serverOffline, severity: .warning,
        titleKey: "peppy.server_offline_title", titleArgument: nil,
        bodyKey: "peppy.server_offline_body", bodyArgument: nil,
        action: nil, snoozeKey: nil
    )

    private static func accountPresentation(_ notice: AccountNoticeSnapshot, now: Date) -> SystemRibbonPresentation? {
        let stem = "peppy.account_\(notice.kind.rawValue)"
        // The deadline is formatted only while it is still in the future, in the device's zone and locale.
        let futureDeadline: String? = notice.deadline
            .map { Date(timeIntervalSince1970: TimeInterval($0)) }
            .flatMap { $0 > now ? $0.formatted(date: .abbreviated, time: .omitted) : nil }
        var titleArgument: String?
        var bodyKey = "\(stem)_body"
        var bodyArgument: String?
        switch notice.kind {
        case .paymentFailedGrace:
            if let futureDeadline { bodyArgument = futureDeadline } else { bodyKey = "\(stem)_body_nodate" }
        case .endingScheduled:
            // The copy names its date in the title and body, so a missing or past date cannot be shown.
            guard let futureDeadline else { return nil }
            titleArgument = futureDeadline
            bodyArgument = futureDeadline
        default:
            break
        }
        let action = notice.manageUrl
            .flatMap { URL(string: $0) }
            .map { SystemRibbonPresentation.Action(labelKey: "\(stem)_action", url: $0) }
        return SystemRibbonPresentation(
            source: .account(notice.kind), severity: notice.severity,
            titleKey: "\(stem)_title", titleArgument: titleArgument,
            bodyKey: bodyKey, bodyArgument: bodyArgument,
            action: action, snoozeKey: notice.dismissible ? notice.snoozeKey : nil
        )
    }
}

/// The last successful decision and the enrollment it belongs to. Nothing is stored before a success.
struct StoredAccountAccess: Codable, Equatable, Sendable {
    let identity: EnrollmentIdentity
    let notice: AccountNoticeSnapshot?
    let fetchedAt: Date
}

/// Account access state for the global ribbon. It holds the last good decision and the local snooze map
/// in UserDefaults. Failures never clear the last good decision.
@MainActor @Observable
public final class AccountAccessModel {
    /// The device-authenticated `GET /v1/account/access` result: HTTP status and raw body.
    public typealias Fetch = @Sendable (EnrollmentIdentity) async throws -> (status: UInt16, body: String)

    public static let snoozeInterval: TimeInterval = 24 * 60 * 60
    public static let forcedRefreshInterval: TimeInterval = 15 * 60
    static let stateKey = "peppy.account-access.state.v1"
    static let snoozeStateKey = "peppy.account-access.snoozes.v1"

    private(set) var stored: StoredAccountAccess?
    private(set) var snoozedAt: [String: Date]
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let clock: @Sendable () -> Date
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var lastForcedRefresh: Date?

    public init(defaults: UserDefaults = .standard, clock: @escaping @Sendable () -> Date = { Date() }) {
        self.defaults = defaults
        self.clock = clock
        stored = Self.load(StoredAccountAccess.self, key: Self.stateKey, defaults: defaults)
        snoozedAt = Self.load([String: Date].self, key: Self.snoozeStateKey, defaults: defaults) ?? [:]
    }

    /// The notice to show for this enrollment, or nil when none is stored or it is snoozed.
    public func visibleNotice(for identity: EnrollmentIdentity?, now: Date) -> AccountNoticeSnapshot? {
        guard let identity, let stored, stored.identity == identity, let notice = stored.notice else { return nil }
        return isSnoozed(notice, now: now) ? nil : notice
    }

    /// Snoozes a dismissible notice for 24 hours. Read-only notices and notices in their last 48 hours
    /// are not dismissible, so the call does nothing for them.
    public func dismiss(snoozeKey: String) {
        guard let notice = stored?.notice, notice.dismissible, notice.snoozeKey == snoozeKey else { return }
        let date = clock()
        snoozedAt = snoozedAt.filter { date.timeIntervalSince($0.value) < Self.snoozeInterval }
        snoozedAt[snoozeKey] = date
        persist(snoozedAt, key: Self.snoozeStateKey)
    }

    /// Fetches the decision and applies it. A transport failure, or a 503-style answer, keeps the last good result.
    public func refresh(identity: EnrollmentIdentity, trustedOrigins: [String], fetch: Fetch) async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        let response: (status: UInt16, body: String)
        do {
            response = try await fetch(identity)
        } catch {
            return
        }
        guard !Task.isCancelled else { return }
        let date = clock()
        let result = accountAccessFromResponse(
            status: response.status, body: response.body,
            nowUnix: Int64(date.timeIntervalSince1970), trustedOrigins: trustedOrigins
        )
        switch result {
        case .notice(let notice):
            store(identity: identity, notice: AccountNoticeSnapshot(native: notice), at: date)
        case .none, .unsupported:
            store(identity: identity, notice: nil, at: date)
        case .temporarilyUnavailable:
            return
        }
    }

    /// Refreshes after a coded `operation_not_permitted` 403. It is skipped when the cached notice is already
    /// read-only, and runs at most once per `forcedRefreshInterval`.
    public func refreshAfterPolicyDenial(identity: EnrollmentIdentity, trustedOrigins: [String], fetch: Fetch) async {
        let date = clock()
        if let lastForcedRefresh, date.timeIntervalSince(lastForcedRefresh) < Self.forcedRefreshInterval { return }
        if let stored, stored.identity == identity, stored.notice?.severity == .error { return }
        lastForcedRefresh = date
        await refresh(identity: identity, trustedOrigins: trustedOrigins, fetch: fetch)
    }

    private func isSnoozed(_ notice: AccountNoticeSnapshot, now: Date) -> Bool {
        guard notice.dismissible, let at = snoozedAt[notice.snoozeKey] else { return false }
        return now.timeIntervalSince(at) < Self.snoozeInterval
    }

    private func store(identity: EnrollmentIdentity, notice: AccountNoticeSnapshot?, at date: Date) {
        stored = StoredAccountAccess(identity: identity, notice: notice, fetchedAt: date)
        persist(stored, key: Self.stateKey)
    }

    private func persist<Value: Encodable>(_ value: Value?, key: String) {
        guard let value, let data = try? JSONEncoder().encode(value) else { return }
        defaults.set(data, forKey: key)
    }

    private static func load<Value: Decodable>(_ type: Value.Type, key: String, defaults: UserDefaults) -> Value? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(type, from: $0) }
    }
}

extension AccountNoticeKind {
    init(native kind: NativeAccountNoticeKind) {
        switch kind {
        case .paymentFailedGrace: self = .paymentFailedGrace
        case .endingScheduled: self = .endingScheduled
        case .readOnlyPastDue: self = .readOnlyPastDue
        case .readOnlyCanceled: self = .readOnlyCanceled
        case .readOnlyIncomplete: self = .readOnlyIncomplete
        case .readOnlyNoSubscription: self = .readOnlyNoSubscription
        case .readOnlyTesterIneligible: self = .readOnlyTesterIneligible
        case .readOnlyAccountDeleting: self = .readOnlyAccountDeleting
        case .readOnlyOther: self = .readOnlyOther
        }
    }
}

extension AccountNoticeSnapshot {
    init(native notice: NativeAccountNotice) {
        kind = AccountNoticeKind(native: notice.kind)
        switch notice.severity {
        case .warning: severity = .warning
        case .error: severity = .error
        }
        deadline = notice.deadline
        manageUrl = notice.manageUrl
        dismissible = notice.dismissible
        snoozeKey = notice.snoozeKey
    }
}

extension ClientError {
    /// A JSON-coded 403 `operation_not_permitted`: a policy pause, not an authentication failure. The rule
    /// itself lives in Rust (`is_account_policy_denial`). `ClientError.server` keeps only the parsed `code`,
    /// not the body, so this re-serializes `{"code": ...}` into the body shape the Rust check parses.
    /// That keeps one definition of the rule instead of duplicating the string test in Swift.
    public var isPolicyDenial: Bool {
        guard case .server(403, let code?) = self,
              let data = try? JSONSerialization.data(withJSONObject: ["code": code]),
              let body = String(data: data, encoding: .utf8) else { return false }
        return isAccountPolicyDenial(status: 403, body: body)
    }
}

extension SyncReport {
    /// True when the pass stopped its uploads on a policy pause. The envelopes stay queued.
    public var refusedByAccountPolicy: Bool { uploadRefused?.isPolicyDenial ?? false }
}
