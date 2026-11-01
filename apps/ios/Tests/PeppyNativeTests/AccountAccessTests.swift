import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

/// Account notices through the real shared Rust decision (`accountAccessFromResponse`), so copy keys and
/// snooze rules are checked against what Rust returns.
@MainActor @Suite struct AccountAccessTests {
    static let identity = EnrollmentIdentity(origin: "https://peppy.pro", vaultId: "vault-test", deviceId: "device-test")
    static let trusted = ["https://peppy.pro"]
    static let start = Date(timeIntervalSince1970: 1_800_000_000)
    static let day: TimeInterval = 24 * 60 * 60

    final class TestClock: @unchecked Sendable {
        var now: Date
        init(_ now: Date) { self.now = now }
    }

    /// Replays one scripted response and counts calls.
    final class ScriptedFetch: @unchecked Sendable {
        var outcome: Result<(status: UInt16, body: String), ClientError>
        private(set) var calls = 0

        init(_ outcome: Result<(status: UInt16, body: String), ClientError>) { self.outcome = outcome }

        func fetch(_ identity: EnrollmentIdentity) async throws -> (status: UInt16, body: String) {
            calls += 1
            return try outcome.get()
        }
    }

    static func accessBody(
        access: String, state: String, reason: String?, deadline: Int64?, manageUrl: String? = "https://peppy.pro/account"
    ) -> String {
        var object: [String: Any] = ["access": access, "state": state]
        object["reason"] = reason ?? NSNull()
        if let deadline { object["deadline"] = deadline } else { object["deadline"] = NSNull() }
        object["manage_url"] = manageUrl ?? NSNull()
        let data = try! JSONSerialization.data(withJSONObject: object)
        return String(data: data, encoding: .utf8)!
    }

    static func readOnly(reason: String?, manageUrl: String? = "https://peppy.pro/account") -> ScriptedFetch {
        ScriptedFetch(.success((200, accessBody(access: "read_only", state: "read_only", reason: reason, deadline: nil, manageUrl: manageUrl))))
    }

    static func makeDefaults() -> UserDefaults {
        UserDefaults(suiteName: "account-access-tests-\(UUID().uuidString)")!
    }

    static func makeModel(defaults: UserDefaults, clock: TestClock) -> AccountAccessModel {
        AccountAccessModel(defaults: defaults, clock: { clock.now })
    }

    /// Refreshes once and returns what the ribbon would resolve to.
    static func presentation(_ model: AccountAccessModel, clock: TestClock, fetch: ScriptedFetch) async -> SystemRibbonPresentation? {
        await model.refresh(identity: identity, trustedOrigins: trusted, fetch: fetch.fetch)
        return SystemRibbonResolver.presentation(
            serverOffline: false, notice: model.visibleNotice(for: identity, now: clock.now), now: clock.now
        )
    }

    // MARK: Presentation

    @Test func readOnlyReasonsMapToTheirOwnCopyStem() async {
        let cases: [(reason: String?, kind: AccountNoticeKind, stem: String)] = [
            ("past_due", .readOnlyPastDue, "read_only_past_due"),
            ("canceled", .readOnlyCanceled, "read_only_canceled"),
            ("incomplete", .readOnlyIncomplete, "read_only_incomplete"),
            ("no_subscription", .readOnlyNoSubscription, "read_only_no_subscription"),
            ("tester_ineligible", .readOnlyTesterIneligible, "read_only_tester_ineligible"),
            ("account_deleting", .readOnlyAccountDeleting, "read_only_account_deleting"),
            (nil, .readOnlyOther, "read_only_other"),
            ("some_future_reason", .readOnlyOther, "read_only_other"),
        ]
        for testCase in cases {
            let clock = TestClock(Self.start)
            let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
            let shown = await Self.presentation(model, clock: clock, fetch: Self.readOnly(reason: testCase.reason))
            #expect(shown?.source == .account(testCase.kind))
            #expect(shown?.severity == .error)
            #expect(shown?.titleKey == "peppy.account_\(testCase.stem)_title")
            #expect(shown?.bodyKey == "peppy.account_\(testCase.stem)_body")
            #expect(shown?.titleArgument == nil && shown?.bodyArgument == nil)
            #expect(shown?.action?.labelKey == "peppy.account_\(testCase.stem)_action")
            #expect(shown?.action?.url == URL(string: "https://peppy.pro/account"))
            #expect(shown?.snoozeKey == nil, "read-only is never dismissible")
        }
    }

    @Test func readOnlyWithoutAManageUrlHasNoAction() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let shown = await Self.presentation(model, clock: clock, fetch: Self.readOnly(reason: "past_due", manageUrl: nil))
        #expect(shown != nil)
        #expect(shown?.action == nil)
    }

    @Test func paymentGraceWithAFutureDeadlineNamesTheDate() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: deadline))))
        let shown = await Self.presentation(model, clock: clock, fetch: fetch)
        #expect(shown?.source == .account(.paymentFailedGrace))
        #expect(shown?.severity == .warning)
        #expect(shown?.titleKey == "peppy.account_payment_failed_grace_title")
        #expect(shown?.bodyKey == "peppy.account_payment_failed_grace_body")
        #expect(shown?.bodyArgument?.isEmpty == false)
        #expect(shown?.snoozeKey != nil, "more than 48 hours before the deadline is dismissible")
    }

    @Test func paymentGraceWithoutADeadlineUsesTheNoDateCopy() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: nil))))
        let shown = await Self.presentation(model, clock: clock, fetch: fetch)
        #expect(shown?.bodyKey == "peppy.account_payment_failed_grace_body_nodate")
        #expect(shown?.bodyArgument == nil)
        #expect(shown?.snoozeKey != nil)
    }

    @Test func paymentGraceInItsLast48HoursIsNotDismissible() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 24 * 60 * 60)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: deadline))))
        let shown = await Self.presentation(model, clock: clock, fetch: fetch)
        #expect(shown?.bodyArgument?.isEmpty == false)
        #expect(shown?.snoozeKey == nil)
    }

    @Test func endingScheduledNamesItsDateInTitleAndBody() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "ending_scheduled", reason: "canceled", deadline: deadline))))
        let shown = await Self.presentation(model, clock: clock, fetch: fetch)
        #expect(shown?.source == .account(.endingScheduled))
        #expect(shown?.severity == .warning)
        #expect(shown?.titleKey == "peppy.account_ending_scheduled_title")
        #expect(shown?.titleArgument?.isEmpty == false)
        #expect(shown?.bodyKey == "peppy.account_ending_scheduled_body")
        #expect(shown?.bodyArgument?.isEmpty == false)
        #expect(shown?.action?.labelKey == "peppy.account_ending_scheduled_action")
    }

    @Test func endingScheduledHidesOnceItsDateHasPassed() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "ending_scheduled", reason: "canceled", deadline: deadline))))
        let shown = await Self.presentation(model, clock: clock, fetch: fetch)
        #expect(shown != nil)
        clock.now = Self.start.addingTimeInterval(6 * Self.day)
        #expect(SystemRibbonResolver.presentation(serverOffline: false, notice: model.visibleNotice(for: Self.identity, now: clock.now), now: clock.now) == nil)
    }

    @Test func serverOfflinePreemptsTheAccountNotice() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        _ = await Self.presentation(model, clock: clock, fetch: Self.readOnly(reason: "past_due"))
        let notice = model.visibleNotice(for: Self.identity, now: clock.now)
        let offline = SystemRibbonResolver.presentation(serverOffline: true, notice: notice, now: clock.now)
        #expect(offline?.source == .serverOffline)
        #expect(offline?.titleKey == "peppy.server_offline_title")
        #expect(offline?.bodyKey == "peppy.server_offline_body")
        #expect(offline?.action == nil && offline?.snoozeKey == nil)
        #expect(SystemRibbonResolver.presentation(serverOffline: false, notice: nil, now: clock.now) == nil)
    }

    @Test func offlineOnlyShowsWhenNothingIsStored() {
        let presentation = SystemRibbonResolver.presentation(serverOffline: true, notice: nil, now: Self.start)
        #expect(presentation?.kindIdentifier == "server_offline")
    }

    // MARK: Snooze

    @Test func dismissingAWarningSnoozesItFor24Hours() async {
        let clock = TestClock(Self.start)
        let defaults = Self.makeDefaults()
        let model = Self.makeModel(defaults: defaults, clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: deadline))))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: fetch.fetch)
        let notice = model.visibleNotice(for: Self.identity, now: clock.now)
        #expect(notice != nil)

        model.dismiss(snoozeKey: notice!.snoozeKey)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) == nil)

        clock.now = Self.start.addingTimeInterval(23 * 60 * 60)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) == nil)

        clock.now = Self.start.addingTimeInterval(25 * 60 * 60)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) != nil)
    }

    @Test func snoozeSurvivesARelaunch() async {
        let clock = TestClock(Self.start)
        let defaults = Self.makeDefaults()
        let model = Self.makeModel(defaults: defaults, clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let fetch = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: deadline))))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: fetch.fetch)
        model.dismiss(snoozeKey: model.visibleNotice(for: Self.identity, now: clock.now)!.snoozeKey)

        let relaunched = Self.makeModel(defaults: defaults, clock: clock)
        #expect(relaunched.visibleNotice(for: Self.identity, now: clock.now) == nil)
    }

    @Test func readOnlyNoticeCannotBeSnoozed() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: Self.readOnly(reason: "canceled").fetch)
        let notice = model.visibleNotice(for: Self.identity, now: clock.now)
        #expect(notice?.dismissible == false)
        model.dismiss(snoozeKey: notice!.snoozeKey)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) != nil)
    }

    // MARK: Cache, failures and identity

    @Test func nothingShowsBeforeTheFirstSuccess() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let failing = ScriptedFetch(.failure(.network(code: -1009)))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: failing.fetch)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) == nil)
    }

    @Test func aFailureOrUnavailableAnswerKeepsTheLastGoodNotice() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: Self.readOnly(reason: "past_due").fetch)

        let failing = ScriptedFetch(.failure(.network(code: -1009)))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: failing.fetch)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) != nil)

        let unavailable = ScriptedFetch(.success((503, "")))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: unavailable.fetch)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) != nil)
    }

    @Test func unsupportedServersClearTheNotice() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: Self.readOnly(reason: "past_due").fetch)
        let older = ScriptedFetch(.success((404, "")))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: older.fetch)
        #expect(model.visibleNotice(for: Self.identity, now: clock.now) == nil)
    }

    @Test func aCachedNoticeSurvivesARelaunchUntilTheNextRefresh() async {
        let clock = TestClock(Self.start)
        let defaults = Self.makeDefaults()
        let model = Self.makeModel(defaults: defaults, clock: clock)
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: Self.readOnly(reason: "past_due").fetch)
        let relaunched = Self.makeModel(defaults: defaults, clock: clock)
        #expect(relaunched.visibleNotice(for: Self.identity, now: clock.now)?.kind == .readOnlyPastDue)
    }

    @Test func aNoticeIsNeverShownForAnotherEnrollment() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: Self.readOnly(reason: "past_due").fetch)
        let other = EnrollmentIdentity(origin: "https://peppy.pro", vaultId: "other-vault", deviceId: "other-device")
        #expect(model.visibleNotice(for: other, now: clock.now) == nil)
        #expect(model.visibleNotice(for: nil, now: clock.now) == nil)
    }

    // MARK: Forced refresh after a policy pause

    @Test func policyDenialRefreshIsDebouncedToOnePerFifteenMinutes() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let deadline = Int64(Self.start.timeIntervalSince1970 + 5 * Self.day)
        let warning = ScriptedFetch(.success((200, Self.accessBody(access: "read_write", state: "payment_failed_grace", reason: nil, deadline: deadline))))
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: warning.fetch)
        #expect(warning.calls == 1)

        await model.refreshAfterPolicyDenial(identity: Self.identity, trustedOrigins: Self.trusted, fetch: warning.fetch)
        #expect(warning.calls == 2)
        await model.refreshAfterPolicyDenial(identity: Self.identity, trustedOrigins: Self.trusted, fetch: warning.fetch)
        #expect(warning.calls == 2, "a second denial inside 15 minutes is debounced")

        clock.now = Self.start.addingTimeInterval(16 * 60)
        await model.refreshAfterPolicyDenial(identity: Self.identity, trustedOrigins: Self.trusted, fetch: warning.fetch)
        #expect(warning.calls == 3)
    }

    @Test func policyDenialRefreshIsSkippedWhileAlreadyReadOnly() async {
        let clock = TestClock(Self.start)
        let model = Self.makeModel(defaults: Self.makeDefaults(), clock: clock)
        let readOnly = Self.readOnly(reason: "past_due")
        await model.refresh(identity: Self.identity, trustedOrigins: Self.trusted, fetch: readOnly.fetch)
        await model.refreshAfterPolicyDenial(identity: Self.identity, trustedOrigins: Self.trusted, fetch: readOnly.fetch)
        #expect(readOnly.calls == 1)
    }

    @Test func aCodedOperationNotPermitted403IsThePolicyTrigger() {
        #expect(ClientError.server(status: 403, code: "operation_not_permitted").isPolicyDenial)
        #expect(!ClientError.server(status: 403, code: "something_else").isPolicyDenial)
        #expect(!ClientError.server(status: 403, code: nil).isPolicyDenial, "a bodiless 403 is not a policy pause")
        #expect(!ClientError.server(status: 409, code: "operation_not_permitted").isPolicyDenial)
        #expect(!ClientError.forbidden.isPolicyDenial)
    }

    @Test func aSyncPassThatRefusedUploadsOnPolicyFlagsTheRefresh() {
        var refused = SyncReport()
        refused.uploadRefused = .server(status: 403, code: "operation_not_permitted")
        #expect(refused.refusedByAccountPolicy)
        #expect(!SyncReport().refusedByAccountPolicy)
    }
}
