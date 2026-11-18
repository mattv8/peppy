import Foundation
#if canImport(PeppyBindings)
import PeppyBindings
#endif

/// Limits for one foreground pass. Requests and apply rounds are shared by every step, so repeated
/// resync answers cannot extend a pass. Anything left over is picked up by the next pass.
public struct SyncBudget: Sendable {
    /// Server requests per pass (uploads, replay pages, snapshot cuts and pages).
    public var maxRequests = 24
    /// Upload requests per pass, within `maxRequests`, so receive is never starved.
    public var maxUploads = 16
    /// Records per server page (the server allows at most 200; the core accepts at most 500).
    public var pageLimit = 100
    /// `applyPending` calls per pass.
    public var maxApplyRounds = 16
    public var applyLimit: UInt64 = 500
    /// Snapshot transfer and drain time per foreground pass. Ordinary replay still uses request limits.
    public var snapshotTimeLimit: TimeInterval = 60

    public init() {}
}

public struct SyncReport: Equatable, Sendable {
    public var uploaded = 0
    /// Why uploads stopped early, if the server refused an envelope. The envelope stays queued
    /// (unacknowledged) and receive still runs so the core can reconcile it.
    public var uploadRefused: ClientError?
    public var journaled = 0
    public var duplicates = 0
    public var quarantinedOnIngest = 0
    public var applied: UInt64 = 0
    /// Journal rows an authoritative snapshot proved superseded (suppressed, not applied).
    public var superseded: UInt64 = 0
    public var quarantined: UInt64 = 0
    public var waitingForKeys: UInt64 = 0
    /// Records downloaded from the server in this pass; records verified from the local journal are not counted.
    public var snapshotRecords = 0
    public var snapshotPublished = false
    public var snapshotRemaining: UInt64 = 0
    /// Commands addressed to this device. They are counted, never executed (see `TelephonyEligibility`).
    public var carrierCommandsNotExecuted = 0
    public var receiveCursor = "0"
    /// Server requests this pass made, ordinary and snapshot traffic together.
    public var requests = 0
    public var applyRounds = 0
    /// False when the budget ran out; the next pass continues.
    public var complete = true
    /// True when the pass advanced durable work: an upload, a journaled or quarantined event,
    /// staged or published snapshot records, or applied, drained, superseded or quarantined rows.
    public var progressed = false
    /// The core latched a contact/notification projection repair that has not promoted yet.
    public var contactRepairRequired = false
    /// A repair snapshot was started in this pass.
    public var repairSnapshotStarted = false
    /// The newest authoritative projection failed (`snapshot_projection_status` reason).
    public var projectionFailure: String?
    /// The server offers no compaction snapshot, so a latched repair cannot run yet.
    public var repairUnavailable = false
    /// A snapshot was rejected as larger than this app version can import.
    public var historyTooLarge = false
}

public enum SetupProgress: Equatable, Sendable {
    case checking
    case downloading(staged: UInt64, expected: UInt64)
    case organizing(remaining: UInt64?)
}

/// Thrown only by the manifest GET when the server answers 404 or 405. `importSnapshot` alone catches it.
private struct ManifestRouteUnavailable: Error {}

/// One bounded, origin-bound foreground pass:
/// upload sealed outbox → finish staged snapshot → drain published snapshot → live replay (or a new
/// snapshot on `resync_required`) → bounded apply. All message state goes through the core facade.
/// Cancelling the calling task stops the pass between steps and cancels in-flight requests.
struct ForegroundSync: Sendable {
    let client: NativeClient
    let server: ServerClient
    let vaultId: String
    var budget = SyncBudget()
    let shouldBeginSnapshot: @Sendable () -> Bool
    let didRejectSnapshotAsTooLarge: @Sendable (SnapshotTooLargeStore.Rejection) -> Void
    let didBeginSnapshot: @Sendable () -> Void
    /// Monotonic nanoseconds for the snapshot time budget; wall-clock jumps must not shorten or extend it.
    let clock: @Sendable () -> UInt64

    init(
        client: NativeClient, server: ServerClient, vaultId: String, budget: SyncBudget = SyncBudget(),
        shouldBeginSnapshot: @escaping @Sendable () -> Bool = { true },
        didRejectSnapshotAsTooLarge: @escaping @Sendable (SnapshotTooLargeStore.Rejection) -> Void = { _ in },
        didBeginSnapshot: @escaping @Sendable () -> Void = {},
        clock: @escaping @Sendable () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }
    ) {
        self.client = client
        self.server = server
        self.vaultId = vaultId
        self.budget = budget
        self.shouldBeginSnapshot = shouldBeginSnapshot
        self.didRejectSnapshotAsTooLarge = didRejectSnapshotAsTooLarge
        self.didBeginSnapshot = didBeginSnapshot
        self.clock = clock
    }

    /// The server clamps `limit` to its maximum manifest page of 5,000 entries.
    private static let manifestPageLimit = 5_000
    /// About 125 bytes per entry, so a 5,000-entry page is about 625 KB; 2 MiB leaves margin.
    private static let maxManifestPageBytes = 2 * 1024 * 1024

    /// Per-pass mutable state.
    private struct Pass {
        var report = SyncReport()
        var meter: RequestMeter
        /// Snapshot page traffic is bounded by foreground time rather than the ordinary replay allowance.
        var snapshotMeter = RequestMeter(limit: .max)
        /// Ordinary replay apply rounds. Drain rounds are counted apart so a long drain never uses this allowance.
        var applyRounds = 0
        var drainRounds = 0
        /// Monotonic start of this pass's snapshot work; nil until a snapshot import or drain begins.
        var snapshotStartedAt: UInt64?
        /// Set when a snapshot could not begin or continue in this pass. `receive` then returns
        /// instead of retrying the 409 that asked for it.
        var snapshotDeferred = false
        /// Set once the manifest route answered 404/405; later begins in this pass are full imports.
        var manifestRouteMissing = false
    }

    func run() async throws -> SyncReport {
        var pass = Pass(meter: RequestMeter(limit: budget.maxRequests))
        do {
            try await probeSnapshotCapability(&pass)
            try await uploadOutbox(&pass)
            if let staged = try client.snapshotProgress() {
                try await importSnapshot(resuming: staged, restartOnResync: true, &pass)
            }
            // Published snapshot records must drain before live replay continues.
            try await applyPending(&pass, drainingSnapshot: try client.snapshotProgress() != nil)
            try await repairIfRequired(&pass)
            if try !snapshotPending(pass) { try await receive(&pass) }
            try await applyPending(&pass, drainingSnapshot: pass.report.snapshotRemaining > 0)
        } catch ClientError.budgetExhausted {
            pass.report.complete = false
        }
        if try snapshotPending(pass) { pass.report.complete = false }
        pass.report.contactRepairRequired = try client.contactRepairRequired()
        if let status = try client.snapshotProjectionStatus(), status.state == .failed {
            pass.report.projectionFailure = status.reason ?? "failed"
        }
        pass.report.carrierCommandsNotExecuted = try client.pendingCommands().count
        pass.report.receiveCursor = try client.receiveCursor()
        pass.report.requests = pass.meter.used + pass.snapshotMeter.used
        pass.report.applyRounds = pass.applyRounds + pass.drainRounds
        return pass.report
    }

    /// A metadata-only probe keeps compactable producers fail-closed when no resync is needed.
    private func probeSnapshotCapability(_ pass: inout Pass) async throws {
        do {
            try await server.postNoContent("/v1/compaction/capability", meter: &pass.meter)
        } catch let error as ClientError {
            switch error {
            case .unauthorized, .forbidden, .canceled: throw error
            default:
                _ = try client.setServerCompactionState(supported: false, active: false)
                return
            }
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            // A 404 is an older server; unavailable capability negotiation remains fail-closed.
            _ = try client.setServerCompactionState(supported: false, active: false)
            return
        }
        let cut: [String: Any]
        do {
            cut = try await server.get("/v1/snapshot", limit: ServerClient.maxSmallBytes, meter: &pass.meter)
        } catch let error as ClientError {
            switch error {
            case .unauthorized, .forbidden, .canceled: throw error
            default:
                _ = try client.setServerCompactionState(supported: false, active: false)
                return
            }
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            _ = try client.setServerCompactionState(supported: false, active: false)
            return
        }
        let supported = try compactionGeneration(cut) != nil
        let active: Bool
        if let value = cut["compaction_active"] {
            guard let enabled = value as? Bool else { throw ClientError.invalidResponse("compaction_active") }
            active = enabled
        } else {
            active = false
        }
        // This also performs one bounded local frontier-backfill step on each successful probe.
        _ = try client.setServerCompactionState(supported: supported, active: active)
    }

    private func compactionGeneration(_ cut: [String: Any]) throws -> String? {
        guard let supported = cut["compaction_supported"] else { return nil }
        guard let enabled = supported as? Bool else { throw ClientError.invalidResponse("compaction_supported") }
        guard enabled else { return nil }
        let value = try cut.string("compaction_generation")
        guard let generation = UInt64(value), String(generation) == value else {
            throw ClientError.invalidResponse("compaction_generation")
        }
        return value
    }

    private func uploadOutbox(_ pass: inout Pass) async throws {
        let batch = try client.pendingOutboxJsonBatch(limit: UInt64(budget.maxUploads) + 1)
        if batch.count > budget.maxUploads { pass.report.complete = false }
        for json in batch.prefix(budget.maxUploads) {
            try Task.checkCancellation()
            let data = Data(json.utf8)
            guard let envelope = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
                throw ClientError.invalidResponse("core outbox envelope")
            }
            let path = envelope["purpose"] as? String == "command" ? "/v1/commands" : "/v1/events"
            do {
                let accepted = try await server.post(path, json: data, preserveCodedForbidden: true, meter: &pass.meter)
                _ = try accepted.decimal("cursor")
            } catch let error as ClientError where Self.refusesEnvelopeOnly(error) {
                // Keep the envelope queued and in order; receive still runs.
                pass.report.uploadRefused = error
                pass.report.complete = false
                return
            }
            try client.ackOutbox(envelopeId: try envelope.string("envelope_id"))
            pass.report.uploaded += 1
            pass.report.progressed = true
        }
    }

    /// Failures that concern one envelope rather than the connection, credential or task.
    /// Authorization, cancellation, origin/redirect violations, network faults and budget
    /// exhaustion still stop the pass.
    static func refusesEnvelopeOnly(_ error: ClientError) -> Bool {
        switch error {
        case .server, .requestTooLarge, .responseTooLarge, .invalidResponse: true
        default: false
        }
    }

    private func receive(_ pass: inout Pass) async throws {
        var after = try localCursor()
        while true {
            let page: [String: Any]
            do {
                page = try await server.get("/v1/events", query: [
                    URLQueryItem(name: "after", value: String(after)),
                    URLQueryItem(name: "limit", value: String(budget.pageLimit)),
                ], limit: ServerClient.maxPageBytes, meter: &pass.meter)
            } catch ClientError.resyncRequired {
                try await startSnapshot(restartOnResync: true, &pass)
                // A pending or deferred snapshot ends this pass; retrying the 409 cannot make progress.
                if try snapshotPending(pass) || pass.snapshotDeferred { return }
                after = try localCursor()
                continue
            }
            var last = after
            for event in try page.objects("events") {
                try Task.checkCancellation()
                let cursor = try event.decimal("cursor")
                guard cursor > last else { throw ClientError.invalidResponse("event cursor order") }
                last = cursor
                let result = try client.ingestRaw(envelopeJson: try event.object("envelope").jsonData(), cursor: String(cursor))
                switch result.state {
                case .journaled:
                    pass.report.journaled += 1
                    pass.report.progressed = true
                case .duplicate: pass.report.duplicates += 1
                case .quarantined:
                    pass.report.quarantinedOnIngest += 1
                    pass.report.progressed = true
                }
            }
            try client.observeServerHighWater(highWaterCursor: String(try page.decimal("high_water_cursor")))
            guard let next = try page.optionalDecimal("next_after") else { return }
            // Continue after the page actually served, even if the core cursor stops at a gap.
            guard next >= last, next > after else { throw ClientError.invalidResponse("next_after") }
            after = next
        }
    }

    private func localCursor() throws -> UInt64 {
        guard let cursor = UInt64(try client.receiveCursor()) else { throw ClientError.invalidResponse("receive cursor") }
        return cursor
    }

    /// A latched repair fetches exactly one fenced (compaction) snapshot per pass, before any
    /// contact effects. A projection that is still draining/staging is left to finish, and a
    /// failure caused by locked keys is reported rather than refetched.
    private func repairIfRequired(_ pass: inout Pass) async throws {
        guard try client.contactRepairRequired(), try !snapshotPending(pass) else { return }
        if let status = try client.snapshotProjectionStatus() {
            switch status.state {
            case .draining, .staging: return
            case .failed where status.reason == "keys_unavailable":
                pass.report.projectionFailure = status.reason
                return
            default: break
            }
        }
        // `startSnapshot` marks the repair started only once a begin really happened.
        try await startSnapshot(restartOnResync: true, requireCompaction: true, &pass)
    }

    /// Fixes a server cut and stages it from the start. A published generation that is still
    /// draining is finished first, because the core refuses a new snapshot until it is.
    private func startSnapshot(restartOnResync: Bool, requireCompaction: Bool = false, _ pass: inout Pass) async throws {
        // A zero snapshot budget (background callers) never begins a cut; the foreground pass does.
        guard budget.snapshotTimeLimit > 0 else {
            deferSnapshot(&pass)
            return
        }
        // Without apply rounds left the drain state is unknown, so no new generation is begun.
        guard pass.applyRounds < budget.maxApplyRounds else { throw ClientError.budgetExhausted }
        guard shouldBeginSnapshot() else {
            pass.report.historyTooLarge = true
            deferSnapshot(&pass)
            return
        }
        // A published generation must drain before a new cut is begun.
        try await applyPending(&pass, drainingSnapshot: pass.report.snapshotRemaining > 0)
        guard pass.report.snapshotRemaining == 0 else {
            deferSnapshot(&pass)
            return
        }
        let cut = try await server.get("/v1/snapshot", limit: ServerClient.maxSmallBytes, meter: &pass.meter)
        guard try cut.integer("snapshot_version") == 1 else { throw ClientError.invalidResponse("snapshot_version") }
        guard UUID(uuidString: try cut.string("vault_id"))?.uuidString.lowercased() == vaultId else {
            throw ClientError.identityMismatch
        }
        let generation = try compactionGeneration(cut)
        if requireCompaction && generation == nil {
            // A legacy snapshot cannot clear the repair latch; wait for a compaction-capable server.
            pass.report.repairUnavailable = true
            pass.report.repairSnapshotStarted = false
            return
        }
        let highWater = String(try cut.decimal("high_water_cursor"))
        let recordCount = try cut.decimal("record_count")
        let manifest = try !pass.manifestRouteMissing && manifestSupported(cut)
        let progress: NativeSnapshotProgress
        do {
            progress = try manifest
                ? client.beginSnapshotWithManifest(highWater: highWater, recordCount: recordCount, serverCompactionGeneration: generation)
                : client.beginSnapshotWithCompaction(highWater: highWater, recordCount: recordCount, purpose: .resync, serverCompactionGeneration: generation)
        } catch let error where Self.isSnapshotTooLarge(error) {
            markHistoryTooLarge(&pass, rejection: .begin)
            return
        }
        didBeginSnapshot()
        if requireCompaction { pass.report.repairSnapshotStarted = true }
        try await importSnapshot(resuming: progress, restartOnResync: restartOnResync, &pass)
    }

    /// Both the begin call and the page/manifest appends report an oversized snapshot with this error.
    private static func isSnapshotTooLarge(_ error: any Error) -> Bool {
        if case .SnapshotTooLarge? = error as? MobileBindingsError { return true }
        return (error as? ClientError) == .snapshotTooLarge
    }

    /// The snapshot cannot be imported by this app version. The core has already failed the staged
    /// generation, so the marker is the only memory that stops the next pass from restaging it.
    private func markHistoryTooLarge(_ pass: inout Pass, rejection: SnapshotTooLargeStore.Rejection) {
        didRejectSnapshotAsTooLarge(rejection)
        pass.report.historyTooLarge = true
        deferSnapshot(&pass)
    }

    /// The snapshot did not begin or continue in this pass; the next foreground pass resumes it.
    private func deferSnapshot(_ pass: inout Pass) {
        pass.snapshotDeferred = true
        pass.report.complete = false
    }

    private func manifestSupported(_ cut: [String: Any]) throws -> Bool {
        guard let supported = cut["manifest_supported"] else { return false }
        guard let enabled = supported as? Bool else { throw ClientError.invalidResponse("manifest_supported") }
        return enabled
    }

    /// A staged generation or undrained published records must finish before live replay continues.
    private func snapshotPending(_ pass: Pass) throws -> Bool {
        try client.snapshotProgress() != nil || pass.report.snapshotRemaining > 0
    }

    private func importSnapshot(
        resuming start: NativeSnapshotProgress, restartOnResync: Bool, _ pass: inout Pass
    ) async throws {
        var progress = start
        if pass.snapshotStartedAt == nil { pass.snapshotStartedAt = clock() }
        while progress.receivedRecords < progress.expectedRecords {
            guard snapshotTimeRemaining(pass) else {
                deferSnapshot(&pass)
                return
            }
            do {
                if progress.manifest {
                    try await appendManifestPage(&progress, &pass)
                } else {
                    _ = try await appendRecordsPage(&progress, limit: min(budget.pageLimit, 200), &pass)
                }
            } catch ClientError.resyncRequired(let reason) {
                // The cut is no longer valid on the server (e.g. rollback); restage once from a new cut.
                guard restartOnResync else { throw ClientError.resyncRequired(reason: reason) }
                return try await startSnapshot(restartOnResync: false, &pass)
            } catch is ManifestRouteUnavailable {
                // Restage as a full import; this pass never begins a manifest generation again, and the resync retry carries over.
                pass.manifestRouteMissing = true
                return try await startSnapshot(restartOnResync: restartOnResync, &pass)
            } catch let error where Self.isSnapshotTooLarge(error) {
                // The append failed after the download reached the byte cap; the core failed the generation.
                markHistoryTooLarge(&pass, rejection: .append)
                return
            }
        }
        _ = try client.finishSnapshot(generation: progress.generation)
        pass.report.snapshotPublished = true
        pass.report.progressed = true
        // Published, nothing drained yet; `applyPending` refines this.
        pass.report.snapshotRemaining = progress.expectedRecords
        try await applyPending(&pass, drainingSnapshot: true)
    }

    /// Verifies one manifest page against the staged journal, then downloads the records it reports missing.
    /// The core consumes the verified prefix, so the next manifest request starts at the new cursor.
    private func appendManifestPage(_ progress: inout NativeSnapshotProgress, _ pass: inout Pass) async throws {
        let page: [String: Any]
        do {
            page = try await server.get("/v1/snapshot/manifest", query: [
                URLQueryItem(name: "high_water", value: progress.highWater),
                URLQueryItem(name: "after", value: progress.lastCursor),
                URLQueryItem(name: "limit", value: String(Self.manifestPageLimit)),
                URLQueryItem(name: "compaction_generation", value: progress.serverCompactionGeneration),
            ], limit: Self.maxManifestPageBytes, meter: &pass.snapshotMeter)
        } catch ClientError.server(let status, _) where status == 404 || status == 405 {
            throw ManifestRouteUnavailable()
        }
        let entries = try page.objects("entries").map { entry in
            NativeSnapshotManifestEntry(
                cursor: String(try entry.decimal("cursor")),
                digest: try entry.string("digest"),
                flags: try manifestFlags(entry)
            )
        }
        guard !entries.isEmpty else { throw ClientError.invalidResponse("short manifest") }
        try Task.checkCancellation()
        let verified = try client.appendSnapshotManifest(generation: progress.generation, entries: entries)
        if verified.progress.receivedRecords > progress.receivedRecords { pass.report.progressed = true }
        progress = verified.progress
        var missing = Int(verified.missing)
        while missing > 0 {
            guard snapshotTimeRemaining(pass) else { return }
            missing -= try await appendRecordsPage(&progress, limit: min(missing, min(budget.pageLimit, 200)), &pass)
        }
    }

    private func manifestFlags(_ entry: [String: Any]) throws -> UInt8 {
        guard let flags = UInt8(exactly: try entry.integer("flags")) else { throw ClientError.invalidResponse("flags") }
        return flags
    }

    /// Downloads one records page after the staged cursor and appends it. Returns the records it carried.
    private func appendRecordsPage(_ progress: inout NativeSnapshotProgress, limit: Int, _ pass: inout Pass) async throws -> Int {
        let page = try await server.get("/v1/snapshot/records", query: [
            URLQueryItem(name: "high_water", value: progress.highWater),
            URLQueryItem(name: "after", value: progress.lastCursor),
            URLQueryItem(name: "limit", value: String(limit)),
            URLQueryItem(name: "compaction_generation", value: progress.serverCompactionGeneration),
        ], limit: ServerClient.maxPageBytes, meter: &pass.snapshotMeter)
        let records = try page.objects("records").map { record in
            NativeRawSnapshotRecord(
                cursor: String(try record.decimal("cursor")),
                envelopeJson: try record.object("envelope").jsonData()
            )
        }
        guard !records.isEmpty else { throw ClientError.invalidResponse("short snapshot") }
        try Task.checkCancellation()
        progress = try client.appendSnapshotRawPage(generation: progress.generation, records: records)
        pass.report.snapshotRecords += records.count
        pass.report.progressed = true
        return records.count
    }

    /// Drains published snapshot records and applies journaled ones. A drain runs until it is idle,
    /// out of time, or a step makes no progress; ordinary apply rounds are capped per pass.
    private func applyPending(_ pass: inout Pass, drainingSnapshot: Bool = false) async throws {
        var draining = drainingSnapshot
        while draining || pass.applyRounds < budget.maxApplyRounds {
            if draining && pass.snapshotStartedAt == nil { pass.snapshotStartedAt = clock() }
            if draining && !snapshotTimeRemaining(pass) {
                pass.report.complete = false
                return
            }
            try Task.checkCancellation()
            // A drain round is bounded by time, so it never spends the ordinary replay allowance.
            if draining { pass.drainRounds += 1 } else { pass.applyRounds += 1 }
            let step = try client.applyPending(limit: budget.applyLimit)
            pass.report.applied += step.applied
            pass.report.superseded += step.superseded
            pass.report.quarantined += step.quarantined
            pass.report.waitingForKeys = step.waitingForKeys
            pass.report.snapshotRemaining = step.snapshotRemaining
            let moved = step.applied > 0 || step.drained > 0 || step.quarantined > 0 || step.superseded > 0
            if moved { pass.report.progressed = true }
            draining = step.snapshotRemaining > 0
            if !moved {
                // Records waiting for keys, or an idle queue: a drain that cannot move ends here,
                // whatever `snapshotRemaining` says, so the next pass retries.
                if draining { pass.report.complete = false }
                return
            }
            await Task.yield()
        }
        pass.report.complete = false
    }

    /// Foreground passes get `snapshotTimeLimit` seconds of snapshot work, measured monotonically. A zero
    /// limit (background callers) never starts snapshot work, so it reports no time left.
    private func snapshotTimeRemaining(_ pass: Pass) -> Bool {
        guard let startedAt = pass.snapshotStartedAt else { return budget.snapshotTimeLimit > 0 }
        let elapsed = Double(clock() &- startedAt) / 1_000_000_000
        return elapsed < budget.snapshotTimeLimit
    }
}

/// Foreground re-run policy: a call repeats a pass only while the last one advanced durable work,
/// no upload was refused, and no history is too large. The pass cap is a backstop, not the stop rule.
public enum ForegroundPassLoop {
    public static let maxPassesPerCall = 8

    public static func shouldRepeat(after report: SyncReport, passesRun: Int, foregroundActive: Bool, cancelled: Bool) -> Bool {
        !report.complete
            && report.progressed
            && report.uploadRefused == nil
            && !report.historyTooLarge
            && foregroundActive
            && !cancelled
            && passesRun < maxPassesPerCall
    }
}

/// Decides when the setup card announces "Setup finished": only after an import that this session
/// observed in progress completes cleanly. A too-large history never announces completion.
public struct SetupProgressTracker: Equatable, Sendable {
    private var importing = false

    public init() {}

    /// Returns true when the setup announcement should fire for this pass.
    public mutating func observe(progress: SetupProgress?, historyTooLarge: Bool, complete: Bool) -> Bool {
        if progress != nil {
            importing = true
            return false
        }
        guard importing, complete || historyTooLarge else { return false }
        importing = false
        return complete && !historyTooLarge
    }
}

/// Core owns durable operation state; the host only performs bounded, fenced HTTP work.
enum AttachmentRetentionPassResult: Sendable, Equatable { case ready, legacy, unresolved }

struct AttachmentRetentionDriver: Sendable {
    let client: NativeClient
    let server: ServerClient
    let state: AttachmentRetentionHTTPState
    let isActive: @Sendable () async -> Bool

    func reconcile(
        meter: inout RequestMeter, allowReadyAdvertisement: Bool
    ) async throws -> AttachmentRetentionPassResult {
        switch state.negotiationState() {
        case .unknown:
            let capability = try attachmentRetentionCapabilityRequest(cleanupReady: false)
            let response = try await send(capability, meter: &meter)
            try await requireActive()
            if response.status == 404 || response.status == 405 {
                state.markLegacy()
                return .legacy
            }
            guard response.status == capability.expectedSuccessStatus else { return .unresolved }
            try client.configureAttachmentRetentionProtocol(enabled: true)
            state.markSupported()
        case .legacy:
            return .legacy
        case .supported:
            if try !client.attachmentRetentionProtocolEnabled() {
                try client.configureAttachmentRetentionProtocol(enabled: true)
                state.invalidateReadyAdvertisement()
            }
        }
        let header = attachmentRetentionProtocolHeader()
        state.remember(name: header.name, value: header.value)
        for _ in 0..<4 {
            let operations = try client.prepareAttachmentRetentionOperations(limit: attachmentRetentionPrepareLimitMax())
            if operations.isEmpty {
                if try !client.attachmentRetentionPreparationPending() { break }
                continue
            }
            for operation in operations {
                try Task.checkCancellation()
                let request = try operation.operation == .register
                    ? attachmentRetentionRegisterRequest(operation: operation)
                    : attachmentRetentionReleaseRequest(operation: operation)
                let response = try await send(request, meter: &meter)
                try await requireActive()
                guard response.status == request.expectedSuccessStatus else {
                    if try exactRemoval(response) {
                        try client.reportAttachmentRemovedByStoragePolicy(attachmentId: operation.attachmentId)
                    }
                    return .unresolved
                }
                let body = try strictResponse(response.body)
                try client.applyAttachmentRetentionResponse(response: try parseAttachmentRetentionResponse(responseJson: body))
            }
        }
        guard try !client.attachmentRetentionPreparationPending() else { return .unresolved }
        try client.markAttachmentRetentionReconciled()
        guard try client.attachmentRetentionReady() else { state.invalidateReadyAdvertisement(); return .unresolved }
        guard allowReadyAdvertisement, state.shouldAdvertiseReady() else { return .ready }
        let request = try attachmentRetentionCapabilityRequest(cleanupReady: true)
        let response = try await send(request, meter: &meter)
        try await requireActive()
        guard response.status == request.expectedSuccessStatus else { return .unresolved }
        state.markReadyAdvertised()
        return .ready
    }

    private func requireActive() async throws {
        try Task.checkCancellation()
        guard await isActive() else { throw ClientError.canceled }
    }

    private func exactRemoval(_ response: HTTPResponse) throws -> Bool {
        guard response.status == 410 else { return false }
        return classifyAttachmentRetentionError(status: 410, responseJson: try strictResponse(response.body)) == .removedByStoragePolicy
    }

    private func strictResponse(_ data: Data) throws -> String {
        guard data.count <= Int(attachmentRetentionResponseMaxBytes()), let value = String(data: data, encoding: .utf8) else {
            throw ClientError.invalidResponse("retention response")
        }
        return value
    }

    private func send(_ request: NativeAttachmentRetentionRequest, meter: inout RequestMeter) async throws -> HTTPResponse {
        guard let limit = Int(exactly: attachmentRetentionResponseReadLimitBytes()) else { throw ClientError.responseTooLarge(limit: Int.max) }
        let body = request.jsonBody.map { Data($0.utf8) }
        var headers: [String: String] = [:]
        if let header = request.protocolHeader { headers[header.name] = header.value }
        let response = try await server.exchange(request.method, request.relativePath, body: body, contentType: body == nil ? nil : "application/json", extraHeaders: headers, limit: limit, meter: &meter)
        if response.status == request.expectedSuccessStatus, request.expectedSuccessStatus == 204, !response.body.isEmpty { throw ClientError.invalidResponse("retention empty response") }
        return response
    }
}
