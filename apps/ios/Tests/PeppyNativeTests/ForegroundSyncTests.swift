import CryptoKit
import Foundation
import PeppyBindings
import Testing
@testable import PeppyNative

/// Bounded foreground sync against the server contract, with real core clients on both ends.
@Suite struct ForegroundSyncTests {
    let passphrase = "io synthetic vault passphrase"

    func capture(_ client: NativeClient, _ body: String) throws -> NativeCaptured {
        try client.captureIncoming(sms: NativeIncomingSms(
            conversationId: nil, senderAddress: "+15550001111", body: body, providerMessageId: UUID().uuidString, imported: false
        ))
    }

    func sync(_ client: NativeClient, _ server: FakeServer, budget: SyncBudget = SyncBudget()) async throws -> SyncReport {
        try await ForegroundSync(client: client, server: serverClient(server), vaultId: server.vaultId, budget: budget).run()
    }

    @Test func outboxUploadsAreAckedAndAnotherDeviceReceivesThem() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        let captured = try capture(producer, "hello from the producer")

        let sent = try await sync(producer, server)
        #expect(sent.uploaded == 1)
        #expect(sent.duplicates == 1, "the producer's own echo is a duplicate")
        #expect(try producer.pendingOutboxJson().isEmpty)
        #expect(sent.receiveCursor == "1")

        let receiver = try openUnlockedClient(server, passphrase: passphrase)
        let received = try await sync(receiver, server)
        #expect(received.journaled == 1 && received.applied == 1 && received.complete)
        let messages = try receiver.messages(conversationId: captured.conversationId)
        #expect(messages.map(\.body) == ["hello from the producer"])

        let again = try await sync(receiver, server)
        #expect(again.journaled == 0 && again.applied == 0 && again.receiveCursor == "1")
        try producer.dispose()
        try receiver.dispose()
    }

    @Test func expiredCursorsResyncThroughAStagedSnapshot() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        for index in 0..<5 { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        server.mutate { $0.replayFloor = 3 }

        let fresh = try openUnlockedClient(server, passphrase: passphrase)
        var budget = SyncBudget()
        budget.pageLimit = 2
        let report = try await sync(fresh, server, budget: budget)
        #expect(server.paths.contains("GET /v1/snapshot"))
        #expect(report.snapshotPublished && report.snapshotRecords == 5 && report.snapshotRemaining == 0)
        #expect(report.applied == 5 && report.receiveCursor == "5" && report.complete)
        #expect(try fresh.listConversations().count == 1)
        try producer.dispose()
        try fresh.dispose()
    }

    @Test func aSnapshotInterruptedByTheBudgetResumesOnTheNextPass() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        for index in 0..<4 { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        server.mutate { $0.replayFloor = 2 }

        let fresh = try openUnlockedClient(server, passphrase: passphrase)
        var budget = SyncBudget()
        budget.pageLimit = 1
        budget.maxRequests = 6 // declaration, probe, replay 409, snapshot cut, two pages
        let partial = try await sync(fresh, server, budget: budget)
        #expect(!partial.complete && !partial.snapshotPublished && partial.requests == 6)
        #expect(try fresh.snapshotProgress()?.receivedRecords == 2)

        budget.maxRequests = 24
        let resumed = try await sync(fresh, server, budget: budget)
        #expect(resumed.snapshotPublished && resumed.complete && resumed.receiveCursor == "4")
        try producer.dispose()
        try fresh.dispose()
    }

    @Test func uploadsAreBoundedPerPass() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "one")
        _ = try capture(client, "two")
        var budget = SyncBudget()
        budget.maxUploads = 1
        let first = try await sync(client, server, budget: budget)
        #expect(first.uploaded == 1 && !first.complete)
        #expect(try client.pendingOutboxJson().count == 1)
        let second = try await sync(client, server, budget: budget)
        #expect(second.uploaded == 1 && second.complete)
        try client.dispose()
    }

    @Test func refusedRedirectsAndForeignOriginsLeaveTheOutboxUnacknowledged() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "kept")

        server.mutate { $0.override = { _ in HTTPResponse(status: 200, url: URL(string: "https://evil.example.test/v1/events"), body: Data("{\"cursor\":\"1\"}".utf8)) } }
        await #expect(throws: ClientError.originMismatch) { try await sync(client, server) }
        #expect(try client.pendingOutboxJson().count == 1)

        let refusing = RefusingTransport()
        await #expect(throws: ClientError.redirectRefused) {
            try await ForegroundSync(client: client, server: serverClient(server, transport: refusing), vaultId: server.vaultId).run()
        }
        #expect(try client.pendingOutboxJson().count == 1)
        try client.dispose()
    }

    @Test func cancellationStopsBeforeAnyRequestOrAck() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "kept")
        let task = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return try await ForegroundSync(client: client, server: serverClient(server), vaultId: server.vaultId).run()
        }
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(server.paths.isEmpty)
        #expect(try client.pendingOutboxJson().count == 1)
        try client.dispose()
    }

    /// Answers only envelope uploads; the capability probe and receive keep their normal routes.
    func answerUploads(_ server: FakeServer, _ status: Int, _ body: String) {
        server.mutate { $0.override = { request in
            request.method == "POST" && request.url.path == "/v1/events"
                ? HTTPResponse(status: status, url: request.url, body: Data(body.utf8))
                : nil
        } }
    }

    /// Requests of one pass: the probe answered normally, then exactly one upload attempt.
    func uploadPass(_ server: FakeServer, since mark: Int) throws -> (upload: Int, paths: [String]) {
        let paths = Array(server.paths.dropFirst(mark))
        #expect(paths.first == "POST /v1/compaction/capability", "the capability probe ran and passed")
        #expect(paths.filter { $0 == "POST /v1/events" }.count == 1, "the pass reached the upload")
        return (try #require(paths.firstIndex(of: "POST /v1/events")), paths)
    }

    @Test func aRefusedUploadStaysQueuedAndReceiveStillRuns() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        let captured = try capture(producer, "from the producer")
        _ = try await sync(producer, server)

        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "refused by the server")
        answerUploads(server, 422, "{\"code\":\"invalid_envelope\"}")
        let mark = server.paths.count
        let report = try await sync(client, server)
        #expect(report.uploadRefused == .server(status: 422, code: "invalid_envelope"))
        #expect(report.uploaded == 0 && !report.complete)
        #expect(report.journaled == 1 && report.applied == 1)
        #expect(try client.messages(conversationId: captured.conversationId).map(\.body) == ["from the producer"])
        #expect(try client.pendingOutboxJson().count == 1, "refused work is kept, not acknowledged")
        let pass = try uploadPass(server, since: mark)
        #expect(pass.paths[(pass.upload + 1)...].contains("GET /v1/events"), "receive ran after the refusal")
        try producer.dispose()
        try client.dispose()
    }

    @Test func coded403And409UploadsStayQueuedWhileAuthAndBodiless403RemainFatal() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        let captured = try capture(producer, "from the producer")
        _ = try await sync(producer, server)

        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "refused by the server")

        for (status, code) in [(403, "operation_not_permitted"), (409, "conflict")] {
            answerUploads(server, status, "{\"code\":\"\(code)\"}")
            let mark = server.paths.count
            let report = try await sync(client, server)
            #expect(report.uploadRefused == .server(status: status, code: code))
            #expect(report.uploaded == 0 && !report.complete)
            #expect(try client.messages(conversationId: captured.conversationId).map(\.body) == ["from the producer"])
            #expect(try client.pendingOutboxJson().count == 1, "refused work is kept, not acknowledged")
            let pass = try uploadPass(server, since: mark)
            #expect(pass.paths[(pass.upload + 1)...].contains("GET /v1/events"), "receive ran after a \(status) refusal")
        }

        for (status, body, fatal) in [(401, "{}", ClientError.unauthorized), (403, "", ClientError.forbidden)] {
            answerUploads(server, status, body)
            let mark = server.paths.count
            await #expect(throws: fatal) { try await sync(client, server) }
            #expect(try client.pendingOutboxJson().count == 1)
            let pass = try uploadPass(server, since: mark)
            #expect(pass.upload == pass.paths.count - 1, "a fatal \(status) upload stops the pass before receive")
        }
        try producer.dispose()
        try client.dispose()
    }

    @Test func capabilityProbeCoded403FailsClosedWhileBodiless403IsFatal() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        let captured = try capture(producer, "from the producer")
        _ = try await sync(producer, server)

        let client = try openUnlockedClient(server, passphrase: passphrase)
        _ = try capture(client, "uploaded despite the policy")
        server.mutate { $0.override = { request in
            request.url.path == "/v1/compaction/capability"
                ? HTTPResponse(status: 403, url: request.url, body: Data("{\"code\":\"operation_not_permitted\"}".utf8))
                : nil
        } }
        var mark = server.paths.count
        let report = try await sync(client, server)
        let readiness = try NativeContactsCoordinator.object(client.contactSyncReadinessJson())
        #expect(readiness["server_supported"] as? Bool == false, "a coded probe denial fails closed as unsupported")
        #expect(report.uploaded == 1)
        #expect(try client.messages(conversationId: captured.conversationId).map(\.body) == ["from the producer"])
        let pass = Array(server.paths.dropFirst(mark))
        #expect(pass.first == "POST /v1/compaction/capability")
        #expect(!pass.contains("GET /v1/snapshot"))
        #expect(pass.contains("POST /v1/events") && pass.contains("GET /v1/events"), "upload and receive still ran")

        _ = try capture(client, "kept")
        server.mutate { $0.override = { request in
            request.url.path == "/v1/compaction/capability" ? HTTPResponse(status: 403, url: request.url, body: Data()) : nil
        } }
        mark = server.paths.count
        await #expect(throws: ClientError.forbidden) { try await sync(client, server) }
        #expect(Array(server.paths.dropFirst(mark)) == ["POST /v1/compaction/capability"])
        #expect(try client.pendingOutboxJson().count == 1)
        try producer.dispose()
        try client.dispose()
    }

    @Test func repeatedResyncAnswersCannotExceedTheRequestBudget() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        for index in 0..<3 { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        server.mutate { $0.replayFloor = 1_000 } // every replay request answers resync_required

        let fresh = try openUnlockedClient(server, passphrase: passphrase)
        var budget = SyncBudget()
        budget.maxRequests = 9
        let before = server.paths.count
        let report = try await sync(fresh, server, budget: budget)
        #expect(!report.complete)
        #expect(report.requests == 9 && server.paths.count - before == 9)
        #expect(report.snapshotPublished && report.receiveCursor == "3")
        try producer.dispose()
        try fresh.dispose()
    }

    @Test func aPublishedSnapshotDrainsBeforeLiveReplay() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        for index in 0..<5 { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        server.mutate { $0.replayFloor = 3 }

        let fresh = try openUnlockedClient(server, passphrase: passphrase)
        var budget = SyncBudget()
        budget.applyLimit = 2
        budget.maxApplyRounds = 2 // drain check, pre-snapshot check; none left to drain
        let first = try await sync(fresh, server, budget: budget)
        #expect(first.snapshotPublished && first.snapshotRemaining == 5 && !first.complete)

        budget.maxApplyRounds = 1

        let before = server.paths.count
        let second = try await sync(fresh, server, budget: budget)
        #expect(second.snapshotRemaining > 0 && !second.complete)
        #expect(
            Array(server.paths.dropFirst(before)) == ["POST /v1/compaction/capability", "GET /v1/snapshot"],
            "the capability probe runs, but live replay stays blocked while published records are undrained"
        )

        let last = try await sync(fresh, server)
        #expect(last.snapshotRemaining == 0 && last.complete && last.receiveCursor == "5")
        #expect(server.paths.count > before)
        #expect(try fresh.listConversations().count == 1)
        try producer.dispose()
        try fresh.dispose()
    }

    @Test func capabilityProbeRequestsAreMeteredAndLegacy404IsNotAuthenticationFailure() async throws {
        let meteredServer = try FakeServer(passphrase: passphrase)
        let meteredClient = try openUnlockedClient(meteredServer, passphrase: passphrase)
        var budget = SyncBudget()
        budget.maxRequests = 2

        let metered = try await sync(meteredClient, meteredServer, budget: budget)
        #expect(!metered.complete && metered.requests == 2)
        #expect(meteredServer.paths == ["POST /v1/compaction/capability", "GET /v1/snapshot"])

        let legacyServer = try FakeServer(passphrase: passphrase)
        let legacyClient = try openUnlockedClient(legacyServer, passphrase: passphrase)
        legacyServer.mutate { $0.override = { request in
            request.url.path == "/v1/compaction/capability"
                ? legacyServer.reply(request, 404, ["code": "not_found"])
                : nil
        } }
        let legacy = try await sync(legacyClient, legacyServer)
        #expect(legacy.complete)
        #expect(legacyServer.paths == ["POST /v1/compaction/capability", "GET /v1/events"])

        let unauthorizedServer = try FakeServer(passphrase: passphrase)
        let unauthorizedClient = try openUnlockedClient(unauthorizedServer, passphrase: passphrase)
        unauthorizedServer.mutate { $0.override = { request in
            request.url.path == "/v1/compaction/capability"
                ? unauthorizedServer.reply(request, 401, ["code": "unauthorized"])
                : nil
        } }
        await #expect(throws: ClientError.unauthorized) { try await sync(unauthorizedClient, unauthorizedServer) }
        #expect(unauthorizedServer.paths == ["POST /v1/compaction/capability"])

        try meteredClient.dispose()
        try legacyClient.dispose()
        try unauthorizedClient.dispose()
    }

    @Test func snapshotCompactionRosterStateIsRecordedWithoutAddingRequests() async throws {
        let server = try FakeServer(passphrase: passphrase)
        server.mutate { $0.compactionActive = false }
        let client = try openUnlockedClient(server, passphrase: passphrase)
        var budget = SyncBudget()
        budget.maxRequests = 2

        let report = try await sync(client, server, budget: budget)
        let readiness = try NativeContactsCoordinator.object(client.contactSyncReadinessJson())

        #expect(!report.complete && report.requests == 2)
        #expect(readiness["server_supported"] as? Bool == true)
        #expect(readiness["server_active"] as? Bool == false)
        #expect(readiness["contacts_ready"] as? Bool == true)
        try client.dispose()
    }

    /// Producer history 1...5; the holder keeps 1...`held` and the replay floor then passes its cursor.
    func staleHolder(_ server: FakeServer, held: Int = 3) async throws -> (holder: NativeClient, conversationId: String) {
        let producer = try openUnlockedClient(server, passphrase: passphrase)
        let first = try capture(producer, "history 0")
        for index in 1..<held { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        let holder = try openUnlockedClient(server, passphrase: passphrase)
        _ = try await sync(holder, server)
        for index in held..<5 { _ = try capture(producer, "history \(index)") }
        _ = try await sync(producer, server)
        server.mutate { $0.replayFloor = 5 }
        try producer.dispose()
        return (holder, first.conversationId)
    }

    /// The query items of each request to `path`, in order.
    func queries(_ server: FakeServer, _ path: String) -> [[String: String]] {
        server.lock.withLock { server.requests }.filter { $0.url.path == path }.map { request in
            Dictionary(uniqueKeysWithValues: (URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems ?? [])
                .map { ($0.name, $0.value ?? "") })
        }
    }

    @Test func aManifestResyncDownloadsOnlyTheRecordsTheDeviceLacks() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server)
        let (holder, conversationId) = try await staleHolder(server)
        let report = try await sync(holder, server)

        let manifest = queries(server, "/v1/snapshot/manifest")
        #expect(manifest.count == 1 && manifest[0]["after"] == "0" && manifest[0]["high_water"] == "5" && manifest[0]["limit"] == "5000")
        let records = queries(server, "/v1/snapshot/records")
        #expect(records.count == 1 && records[0]["after"] == "3" && records[0]["limit"] == "2", "only records 4 and 5 are downloaded")
        #expect(report.snapshotPublished && report.snapshotRecords == 2 && report.snapshotRemaining == 0)
        #expect(report.receiveCursor == "5" && report.complete)
        #expect(try holder.messages(conversationId: conversationId).map(\.body).sorted() == (0..<5).map { "history \($0)" }.sorted())
        try holder.dispose()
    }

    @Test func aServerWithoutManifestSupportKeepsTheFullResyncImport() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let (holder, conversationId) = try await staleHolder(server)
        let mark = server.paths.count
        let report = try await sync(holder, server)

        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/events",
            "GET /v1/snapshot", "GET /v1/snapshot/records", "GET /v1/events",
        ])
        #expect(report.snapshotPublished && report.snapshotRecords == 5 && report.receiveCursor == "5" && report.complete)
        #expect(try holder.messages(conversationId: conversationId).count == 5)
        try holder.dispose()
    }

    @Test func aManifestResyncAnsweredWithResyncRestartsFromAFreshCut() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let resyncOnce = OnceFlag()
        offerManifest(server, manifestAnswer: { request in
            // The first manifest request answers with a resync; the restarted cut then succeeds.
            resyncOnce.take()
                ? server.reply(request, 409, ["code": "resync_required", "reason": "compaction_generation_changed"])
                : nil
        })
        let (holder, _) = try await staleHolder(server)
        let mark = server.paths.count
        let report = try await sync(holder, server)

        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/events",
            "GET /v1/snapshot", "GET /v1/snapshot/manifest",
            "GET /v1/snapshot", "GET /v1/snapshot/manifest", "GET /v1/snapshot/records", "GET /v1/events",
        ], "the resync answer restarts once from a fresh cut, then the manifest completes")
        #expect(queries(server, "/v1/snapshot/manifest").map { $0["after"] } == ["0", "0"])
        #expect(report.snapshotPublished && report.snapshotRecords == 2 && report.receiveCursor == "5" && report.complete)
        try holder.dispose()
    }

    @Test func aStagedManifestGenerationResumesWithTheManifest() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server)
        let (holder, conversationId) = try await staleHolder(server)
        var budget = SyncBudget()
        budget.maxRequests = 5 // capability probe, cut, events resync, cut, manifest; the first records request is refused
        let partial = try await sync(holder, server, budget: budget)
        #expect(!partial.complete && !partial.snapshotPublished)
        let staged = try #require(try holder.snapshotProgress())
        #expect(staged.manifest && staged.receivedRecords == 3 && staged.lastCursor == "3")

        budget.maxRequests = 24
        let mark = server.paths.count
        let resumed = try await sync(holder, server, budget: budget)
        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/snapshot/manifest",
            "GET /v1/snapshot/records", "GET /v1/events",
        ], "the staged generation continues with the manifest, not a new cut")
        #expect(queries(server, "/v1/snapshot/manifest").last?["after"] == "3")
        #expect(resumed.snapshotPublished && resumed.snapshotRecords == 2 && resumed.receiveCursor == "5" && resumed.complete)
        #expect(try holder.messages(conversationId: conversationId).count == 5)
        try holder.dispose()
    }

    @Test func aStagedManifestGenerationFallsBackToTheFullImportWhenTheRouteIsGone() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server)
        let (holder, conversationId) = try await staleHolder(server)
        var budget = SyncBudget()
        budget.maxRequests = 5
        _ = try await sync(holder, server, budget: budget)
        #expect(try holder.snapshotProgress()?.manifest == true)

        offerManifest(server, manifestAnswer: { request in server.reply(request, 404, ["code": "not_found"]) })
        budget.maxRequests = 24
        let mark = server.paths.count
        let resumed = try await sync(holder, server, budget: budget)
        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/snapshot/manifest",
            "GET /v1/snapshot", "GET /v1/snapshot/records", "GET /v1/events",
        ])
        #expect(queries(server, "/v1/snapshot/records").last?["after"] == "0", "the restart is a full import")
        #expect(resumed.snapshotPublished && resumed.snapshotRecords == 5 && resumed.receiveCursor == "5" && resumed.complete)
        #expect(try holder.messages(conversationId: conversationId).count == 5)
        try holder.dispose()
    }

    @Test func anEmptyManifestPageWithNothingMissingFailsTheAttempt() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server, manifestAnswer: { request in
            server.reply(request, 200, ["high_water_cursor": "5", "compaction_generation": "0", "next_after": NSNull(), "entries": []])
        })
        let (holder, _) = try await staleHolder(server)

        await #expect(throws: ClientError.invalidResponse("short manifest")) { try await sync(holder, server) }
        let staged = try #require(try holder.snapshotProgress())
        #expect(staged.manifest && staged.receivedRecords == 0, "no progress is recorded")
        try holder.dispose()
    }

    @Test func aRecordsNotFoundInsideAManifestGenerationFailsWithoutRestartingTheImport() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server, recordsAnswer: { request in server.reply(request, 404, ["code": "not_found"]) })
        let (holder, _) = try await staleHolder(server)
        let mark = server.paths.count

        await #expect(throws: ClientError.server(status: 404, code: "not_found")) { try await sync(holder, server) }
        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/events",
            "GET /v1/snapshot", "GET /v1/snapshot/manifest", "GET /v1/snapshot/records",
        ], "only the manifest route falls back; a records 404 fails as it did before")
        try holder.dispose()
    }

    @Test func aManifestRouteFallbackKeepsTheResyncRetry() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server)
        let (holder, conversationId) = try await staleHolder(server)
        var budget = SyncBudget()
        budget.maxRequests = 5 // stages the manifest generation with nothing appended
        _ = try await sync(holder, server, budget: budget)
        #expect(try holder.snapshotProgress()?.manifest == true)

        let resyncOnce = OnceFlag()
        offerManifest(server, manifestAnswer: { request in server.reply(request, 404, ["code": "not_found"]) },
                      recordsAnswer: { request in
                          resyncOnce.take()
                              ? server.reply(request, 409, ["code": "resync_required", "reason": "cursor_ahead"])
                              : nil
                      })
        budget.maxRequests = 24
        let mark = server.paths.count
        let resumed = try await sync(holder, server, budget: budget)

        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/snapshot/manifest",
            "GET /v1/snapshot", "GET /v1/snapshot/records",
            "GET /v1/snapshot", "GET /v1/snapshot/records", "GET /v1/events",
        ], "the fallback full import is restarted once by a later resync answer, as a resync always is")
        #expect(resumed.snapshotPublished && resumed.snapshotRecords == 5 && resumed.receiveCursor == "5" && resumed.complete)
        #expect(try holder.messages(conversationId: conversationId).count == 5)
        try holder.dispose()
    }

    @Test func theManifestRequestCarriesTheCompactionGenerationLikeTheRecordsRequest() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server, generation: "7")
        let (holder, _) = try await staleHolder(server)
        _ = try await sync(holder, server)

        #expect(queries(server, "/v1/snapshot/manifest").first?["compaction_generation"] == "7")
        #expect(queries(server, "/v1/snapshot/records").first?["compaction_generation"] == "7")
        try holder.dispose()
    }

    @Test func aMissingRunThatEndsAtThePageBoundaryContinuesOnTheNextManifestPage() async throws {
        let server = try FakeServer(passphrase: passphrase)
        offerManifest(server, pageSize: 3) // the fake clamps each page to three entries, as the server clamps to its maximum
        let (holder, conversationId) = try await staleHolder(server, held: 2)
        let mark = server.paths.count
        let report = try await sync(holder, server)

        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/events",
            "GET /v1/snapshot", "GET /v1/snapshot/manifest", "GET /v1/snapshot/records",
            "GET /v1/snapshot/manifest", "GET /v1/snapshot/records", "GET /v1/events",
        ])
        #expect(queries(server, "/v1/snapshot/manifest").map { $0["after"] } == ["0", "3"])
        #expect(queries(server, "/v1/snapshot/records").map { [$0["after"], $0["limit"]] } == [["2", "1"], ["3", "2"]])
        #expect(report.snapshotPublished && report.snapshotRecords == 3 && report.receiveCursor == "5" && report.complete)
        #expect(try holder.messages(conversationId: conversationId).map(\.body).sorted() == (0..<5).map { "history \($0)" }.sorted())
        try holder.dispose()
    }

    @Test func aStagedFullImportResumesAsAFullImportWhenTheServerNowOffersManifests() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let (holder, conversationId) = try await staleHolder(server)
        var budget = SyncBudget()
        budget.pageLimit = 1
        budget.maxRequests = 6 // capability probe, cut, events resync, cut, and two single-record pages
        let partial = try await sync(holder, server, budget: budget)
        #expect(!partial.complete && !partial.snapshotPublished)
        let staged = try #require(try holder.snapshotProgress())
        #expect(!staged.manifest && staged.receivedRecords == 2)

        offerManifest(server) // the server now advertises manifests; the staged generation is a full import
        budget.maxRequests = 24
        let mark = server.paths.count
        let resumed = try await sync(holder, server, budget: budget)

        #expect(Array(server.paths.dropFirst(mark)) == [
            "POST /v1/compaction/capability", "GET /v1/snapshot", "GET /v1/snapshot/records",
            "GET /v1/snapshot/records", "GET /v1/snapshot/records", "GET /v1/events",
        ])
        #expect(queries(server, "/v1/snapshot/manifest").isEmpty)
        #expect(resumed.snapshotPublished && resumed.snapshotRecords == 3 && resumed.receiveCursor == "5" && resumed.complete)
        #expect(try holder.messages(conversationId: conversationId).count == 5)
        try holder.dispose()
    }

    /// `carrier_route_aad_is_byte_stable_fixture` in `crates/protocol/tests/protocol_contract.rs` pins this envelope's
    /// `wire_digest` (READ ONLY reference). The port must reproduce that value, so it cannot drift silently.
    @Test func manifestDigestPortMatchesTheRustWireDigestFixture() throws {
        let envelope: [String: Any] = [
            "protocol_version": 1,
            "envelope_id": "00000000-0000-0000-0000-000000000001",
            "command_id": "00000000-0000-0000-0000-000000000002",
            "vault_id": "00000000-0000-0000-0000-000000000003",
            "producer_device_id": "00000000-0000-0000-0000-000000000004",
            "producer_sequence": "9",
            "key_epoch": 2,
            "crypto_suite": 3,
            "profile_fingerprint": String(repeating: "a", count: 64),
            "purpose": "command",
            "route": ["gateway_device_id": "00000000-0000-0000-0000-000000000005", "subscription_id": "sim:fixture"],
            "ciphertext": "AQID",
        ]
        #expect(try ManifestFixture.digest(envelope) == "128646e3b468bd6b0a322c287296fae9603530595259c4073fc083fa10a6765d")
    }

    @Test func requestAndClientDiagnosticsRedactTheToken() throws {
        let server = try FakeServer(passphrase: passphrase)
        let client = try serverClient(server)
        let request = HTTPRequest(
            method: "GET", url: URL(string: "https://push.example.test/v1/vault")!,
            headers: ["Authorization": "Bearer \(server.token)", "Accept": "application/json"], body: nil, maxResponseBytes: 1
        )
        for value in [request as Any, client as Any] {
            var dumped = ""
            dump(value, to: &dumped)
            for text in [String(describing: value), String(reflecting: value), dumped] {
                #expect(!text.contains(server.token))
            }
        }
        #expect(String(describing: request).contains("application/json"))
    }

    @Test func retentionDriverNegotiatesBeforeForegroundWork() async throws {
        let server = try FakeServer(passphrase: passphrase)
        let client = try openUnlockedClient(server, passphrase: passphrase)
        let transport = RetentionRecordingTransport { request in
            HTTPResponse(status: 204, url: request.url, body: Data())
        }
        var meter = RequestMeter(limit: 8)
        let state = AttachmentRetentionHTTPState()
        let driver = AttachmentRetentionDriver(
            client: client,
            server: try serverClient(server, transport: transport),
            state: state,
            isActive: { true }
        )

        let result = try await driver.reconcile(meter: &meter, allowReadyAdvertisement: false)
        #expect(transport.requests.compactMap(Self.cleanupReady) == [false])
        let finalized = try await driver.reconcile(meter: &meter, allowReadyAdvertisement: true)
        let again = try await driver.reconcile(meter: &meter, allowReadyAdvertisement: true)

        #expect(result == .ready && finalized == .ready && again == .ready)
        #expect(try client.attachmentRetentionProtocolEnabled())
        #expect(transport.requests.first?.url.path == "/v1/devices/self/retention-capability")
        #expect(transport.requests.first?.method == "PUT")
        #expect(transport.requests.first?.headers["Authorization"] == "Bearer \(server.token)")
        let readiness = transport.requests.compactMap(Self.cleanupReady)
        #expect(readiness == [false, true], "ready is advertised once per open transition")
        try client.dispose()
    }

    private static func cleanupReady(_ request: HTTPRequest) -> Bool? {
        guard let body = request.body,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return nil }
        return object["cleanup_ready"] as? Bool
    }
}

/// Adds the manifest route to the fake: `GET /v1/snapshot` advertises it, and `GET /v1/snapshot/manifest`
/// lists the log's cursors and digests, paged like the real route (`pageSize` mimics the server's clamp).
/// `manifestAnswer` and `recordsAnswer` may answer first; the others keep the fake's normal answers.
func offerManifest(
    _ server: FakeServer,
    generation: String = "0",
    pageSize: Int = 5_000,
    manifestAnswer: (@Sendable (HTTPRequest) -> HTTPResponse?)? = nil,
    recordsAnswer: (@Sendable (HTTPRequest) -> HTTPResponse?)? = nil
) {
    server.mutate { $0.override = { request in
        switch (request.method, request.url.path) {
        case ("GET", "/v1/snapshot"):
            return server.reply(request, 200, [
                "snapshot_version": 1, "compaction_supported": true, "compaction_active": server.compactionActive, "compaction_generation": generation,
                "vault_id": server.vaultId, "high_water_cursor": String(server.log.last?.cursor ?? 0),
                "record_count": String(server.log.count), "replay_floor_cursor": String(server.replayFloor),
                "key_epoch": 1, "profile_fingerprint": "unused", "max_page_size": 200,
                "manifest_supported": true, "max_manifest_page_size": 5_000,
            ])
        case ("GET", "/v1/snapshot/manifest"):
            if let answer = manifestAnswer?(request) { return answer }
            let query = Dictionary(uniqueKeysWithValues: (URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems ?? [])
                .map { ($0.name, $0.value ?? "") })
            let high = UInt64(query["high_water"] ?? "0")!
            let after = UInt64(query["after"] ?? "0")!
            let limit = min(Int(query["limit"] ?? "1000")!, pageSize)
            let rows = server.log.filter { $0.cursor > after && $0.cursor <= high }.prefix(limit)
            let next: Any = rows.last.map { $0.cursor < high ? String($0.cursor) as Any : NSNull() } ?? NSNull()
            do {
                // No test envelope carries compaction metadata or an origin tag, so every flag is 0.
                let entries = try rows.map { row -> [String: Any] in
                    ["cursor": String(row.cursor), "digest": try ManifestFixture.digest(row.envelope), "flags": 0]
                }
                return server.reply(request, 200, [
                    "high_water_cursor": String(high), "compaction_generation": generation, "next_after": next, "entries": entries,
                ])
            } catch {
                Issue.record(error)
                return nil
            }
        case ("GET", "/v1/snapshot/records"):
            return recordsAnswer?(request)
        default:
            return nil
        }
    } }
}

/// Answers once: `take()` is true for the first caller only. Override closures run under the fake's lock.
final class OnceFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var spent = false
    func take() -> Bool {
        lock.withLock {
            if spent { return false }
            spent = true
            return true
        }
    }
}

/// The server's `cipher_digest` (`Envelope::wire_digest`): SHA-256 over the binary AAD, then the ciphertext.
/// The bindings expose no digest, so the fake ports the AAD layout of `crates/protocol/src/envelope.rs`.
/// A wrong port makes every entry non-verifying, which the manifest tests would catch.
enum ManifestFixture {
    /// A field the port cannot read; the caller's test fails with it.
    struct Malformed: Error, CustomStringConvertible {
        let field: String
        var description: String { "manifest fixture: malformed \(field)" }
    }

    static func digest(_ envelope: [String: Any]) throws -> String {
        var aad = Data("peppy-envelope-aad-v1\0".utf8)
        aad += try bigEndian(integer(envelope, "protocol_version", UInt16.self))
        aad += try uuid(envelope, "envelope_id")
        aad += try uuid(envelope, "vault_id")
        aad += try uuid(envelope, "producer_device_id")
        aad += try bigEndian(sequence(envelope))
        aad += try bigEndian(integer(envelope, "key_epoch", UInt32.self))
        aad += try bigEndian(integer(envelope, "crypto_suite", UInt16.self))
        aad += try text(envelope, "profile_fingerprint")
        let purpose: UInt8 = envelope["purpose"] as? String == "command" ? 1 : 2
        aad.append(purpose)
        if envelope["command_id"] is String {
            aad.append(1)
            aad += try uuid(envelope, "command_id")
        } else {
            aad.append(0)
        }
        if let route = envelope["route"] as? [String: Any] {
            if route["kind"] as? String == "rcs_link" {
                aad.append(2)
                aad += try uuid(route, "gateway_device_id")
            } else {
                let subscription = try text(route, "subscription_id")
                aad.append(1)
                aad += try uuid(route, "gateway_device_id")
                guard let length = UInt16(exactly: subscription.count) else { throw Malformed(field: "subscription_id") }
                aad += bigEndian(length) + subscription
            }
        } else {
            aad.append(0)
        }
        guard let encoded = envelope["ciphertext"] as? String, let ciphertext = Data(base64Encoded: encoded) else {
            throw Malformed(field: "ciphertext")
        }
        return SHA256.hash(data: aad + ciphertext).map { String(format: "%02x", $0) }.joined()
    }

    private static func integer<T: FixedWidthInteger>(_ object: [String: Any], _ key: String, _: T.Type) throws -> T {
        guard let value = object[key] as? Int, let exact = T(exactly: value) else { throw Malformed(field: key) }
        return exact
    }

    private static func sequence(_ envelope: [String: Any]) throws -> UInt64 {
        guard let text = envelope["producer_sequence"] as? String, let value = UInt64(text) else {
            throw Malformed(field: "producer_sequence")
        }
        return value
    }

    /// Ids are 16 raw bytes in the AAD (`Envelope::aad_bytes`), not their text.
    private static func uuid(_ object: [String: Any], _ key: String) throws -> Data {
        guard let text = object[key] as? String, let id = UUID(uuidString: text) else { throw Malformed(field: key) }
        return withUnsafeBytes(of: id.uuid) { Data($0) }
    }

    private static func text(_ object: [String: Any], _ key: String) throws -> Data {
        guard let text = object[key] as? String else { throw Malformed(field: key) }
        return Data(text.utf8)
    }

    private static func bigEndian<T: FixedWidthInteger>(_ value: T) -> Data { withUnsafeBytes(of: value.bigEndian) { Data($0) } }
}

struct RefusingTransport: HTTPTransport {
    func send(_ request: HTTPRequest) async throws -> HTTPResponse { throw ClientError.redirectRefused }
}

final class RetentionRecordingTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [HTTPRequest] = []
    private let response: @Sendable (HTTPRequest) -> HTTPResponse

    init(response: @escaping @Sendable (HTTPRequest) -> HTTPResponse) { self.response = response }
    var requests: [HTTPRequest] { lock.withLock { recorded } }
    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        lock.withLock { recorded.append(request) }
        return response(request)
    }
}
