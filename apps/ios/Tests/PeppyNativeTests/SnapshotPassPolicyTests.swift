import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

/// Pure foreground-loop, setup-announcement and too-large marker rules, plus the error mapping they depend on.
@Suite struct SnapshotPassPolicyTests {
    private func advancing() -> SyncReport {
        var report = SyncReport()
        report.complete = false
        report.progressed = true
        return report
    }

    @Test func aPassRepeatsOnlyWhileItAdvancesAndNothingIsRefused() {
        let repeats = { (report: SyncReport, passes: Int, active: Bool, cancelled: Bool) in
            ForegroundPassLoop.shouldRepeat(after: report, passesRun: passes, foregroundActive: active, cancelled: cancelled)
        }
        #expect(repeats(advancing(), 1, true, false))

        var idle = advancing()
        idle.progressed = false
        #expect(!repeats(idle, 1, true, false), "a pass that made no progress must not repeat")

        var refused = advancing()
        refused.uploadRefused = .server(status: 500, code: nil)
        #expect(!repeats(refused, 1, true, false), "a refused upload waits for the next foreground pass")

        var tooLarge = advancing()
        tooLarge.historyTooLarge = true
        #expect(!repeats(tooLarge, 1, true, false))

        var finished = advancing()
        finished.complete = true
        #expect(!repeats(finished, 1, true, false))
        #expect(!repeats(advancing(), 1, false, false), "leaving the foreground stops the loop")
        #expect(!repeats(advancing(), 1, true, true), "cancellation stops the loop")
        #expect(!repeats(advancing(), ForegroundPassLoop.maxPassesPerCall, true, false), "the pass cap is a hard backstop")
    }

    @Test func setupAnnouncesOnlyACleanInSessionCompletion() {
        var tracker = SetupProgressTracker()
        let idle = tracker.observe(progress: nil, historyTooLarge: false, complete: true)
        #expect(!idle, "no import was observed")
        let downloading = tracker.observe(progress: .downloading(staged: 1, expected: 4), historyTooLarge: false, complete: false)
        #expect(!downloading)
        let organizing = tracker.observe(progress: .organizing(remaining: 2), historyTooLarge: false, complete: false)
        #expect(!organizing)
        let finished = tracker.observe(progress: nil, historyTooLarge: false, complete: true)
        #expect(finished)
        let again = tracker.observe(progress: nil, historyTooLarge: false, complete: true)
        #expect(!again, "the announcement fires once")
    }

    @Test func aTooLargeHistoryEndsSetupWithoutAnnouncing() {
        var tracker = SetupProgressTracker()
        let downloading = tracker.observe(progress: .downloading(staged: 1, expected: 4), historyTooLarge: false, complete: false)
        #expect(!downloading)
        let tooLarge = tracker.observe(progress: nil, historyTooLarge: true, complete: false)
        #expect(!tooLarge)
        let later = tracker.observe(progress: nil, historyTooLarge: false, complete: true)
        #expect(!later, "the import already ended")
    }

    @Test func tooLargeMarkerBlocksOneBuildForSixHoursAndExpiresOnAClockStepBack() throws {
        let suite = "peppy-marker-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = SnapshotTooLargeStore(defaults: defaults)
        let start = Date(timeIntervalSinceReferenceDate: 1_000_000)
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: start))

        store.mark(vaultID: "vault", build: "1", now: start)
        #expect(!store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: start.addingTimeInterval(60)))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "2", now: start.addingTimeInterval(60)), "a new build retries")
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: start.addingTimeInterval(6 * 60 * 60)))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: start.addingTimeInterval(-60)), "a backwards clock step is expired")
        #expect(store.shouldBeginSnapshot(vaultID: "other", build: "1", now: start.addingTimeInterval(60)), "markers are per vault")

        store.clear(vaultID: "vault")
        #expect(store.marker(for: "vault") == nil)
    }

    private func isolatedStore() throws -> (SnapshotTooLargeStore, UserDefaults, String) {
        let suite = "peppy-marker-kind-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        return (SnapshotTooLargeStore(defaults: defaults), defaults, suite)
    }

    @Test func aBeginRejectionRechecksAfterSixHours() throws {
        let (store, defaults, suite) = try isolatedStore()
        defer { defaults.removePersistentDomain(forName: suite) }
        let start = Date(timeIntervalSinceReferenceDate: 1_000_000)
        store.mark(vaultID: "vault", build: "1", rejection: .begin, now: start)
        let fiveHours = start.addingTimeInterval(5 * 60 * 60)
        let sixHours = start.addingTimeInterval(SnapshotTooLargeStore.beginRecheckInterval)
        #expect(!store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: fiveHours))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: sixHours))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "2", now: fiveHours), "a build change retries at once")
    }

    @Test func anAppendRejectionWaitsSevenDaysAndRetriesAtOnceOnABuildChange() throws {
        let (store, defaults, suite) = try isolatedStore()
        defer { defaults.removePersistentDomain(forName: suite) }
        let start = Date(timeIntervalSinceReferenceDate: 1_000_000)
        store.mark(vaultID: "vault", build: "1", rejection: .append, now: start)
        let sixHours = start.addingTimeInterval(6 * 60 * 60)
        let sixDays = start.addingTimeInterval(6 * 24 * 60 * 60)
        let sevenDays = start.addingTimeInterval(SnapshotTooLargeStore.appendRecheckInterval)
        #expect(SnapshotTooLargeStore.appendRecheckInterval == 7 * 24 * 60 * 60)
        #expect(!store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: sixHours), "no four-times-a-day re-download")
        #expect(!store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: sixDays))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: sevenDays))
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "2", now: sixHours), "a build change retries at once")
    }

    @Test func aMarkerWithoutTheKindFieldDecodesAsABeginRejection() throws {
        let legacy = Data(#"{"checkedAt":1000000,"build":"1"}"#.utf8)
        let marker = try JSONDecoder().decode(SnapshotTooLargeStore.Marker.self, from: legacy)
        #expect(marker.rejection == nil)
        #expect(marker.rejectionKind == .begin)

        let (store, defaults, suite) = try isolatedStore()
        defer { defaults.removePersistentDomain(forName: suite) }
        let stored = try JSONEncoder().encode(["vault": marker])
        defaults.set(stored, forKey: "peppy.snapshot-too-large")
        let start = Date(timeIntervalSinceReferenceDate: 1_000_000)
        #expect(store.shouldBeginSnapshot(vaultID: "vault", build: "1", now: start.addingTimeInterval(SnapshotTooLargeStore.beginRecheckInterval)))
    }

    @Test func wrapMapsTheCoreTooLargeErrorToTheTypedCase() {
        #expect(ClientError.wrap(MobileBindingsError.SnapshotTooLarge) == .snapshotTooLarge)
        #expect(ClientError.wrap(MobileBindingsError.Closed) == .core(.Closed))
        #expect(ClientError.wrap(ClientError.canceled) == .canceled)
        #expect(ClientError.wrap(CancellationError()) == .canceled)
    }
}
