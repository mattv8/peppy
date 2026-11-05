import PeppyBindings
import Testing
@testable import PeppyNative

@Suite struct GoogleMessagesLinkStatusTests {
    private let gatewayID = "00000000-0000-0000-0000-000000000001"

    private func status(
        _ state: NativeRcsLinkState,
        current: Bool = true,
        availability: Bool = true,
        health: Bool = true,
        phoneResponsive: Bool = true,
        expiresAtMs: Int64? = 1
    ) -> NativeRcsLinkStatus {
        NativeRcsLinkStatus(
            state: state,
            current: current,
            availability: availability,
            health: health,
            phoneResponsive: phoneResponsive,
            observedAtMs: 0,
            capabilityRevision: 1,
            featureBits: 0,
            expiresAtMs: expiresAtMs
        )
    }

    @Test func mapsMissingAndUnlinkedStatusesToNoPresentation() {
        #expect(GoogleMessagesLinkPresentation.presentation(for: nil) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.unlinked)) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.unavailable)) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.unlinked, current: false)) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.unavailable, current: false)) == nil)
    }

    @Test func mapsCurrentStatusesAndAttentionHelp() {
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.linked))?.status == .connected)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.linked, phoneResponsive: false))?.status == .phoneNotResponding)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.linked, availability: false))?.status == .unconfirmed)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.linked, health: false))?.status == .unconfirmed)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.needsRelink))?.status == .needsRelink)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.signedOut))?.status == .signedOut)
        #expect(GoogleMessagesLinkStatus.needsRelink.helpKey == "google_messages_link_remote_help_attention")
        #expect(GoogleMessagesLinkStatus.phoneNotResponding.helpKey == "google_messages_link_remote_help_unresponsive")
    }

    @Test func mapsStaleEvidenceToUnconfirmedBeforeLinkHealth() {
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.linked, current: false))?.status == .unconfirmed)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.needsRelink, current: false))?.status == .unconfirmed)
        #expect(GoogleMessagesLinkPresentation.presentation(for: status(.signedOut, current: false))?.status == .unconfirmed)
    }

    @Test func filtersRowsAndKeepsAPresentRemoteGateway() {
        let remoteGateway = DeviceRosterItem(id: gatewayID, role: "gateway", revoked: false, keyEpoch: 1)
        #expect(GoogleMessagesLinkPresentation.presentation(for: remoteGateway, currentDeviceID: nil, status: status(.linked))?.status == .connected)
        #expect(GoogleMessagesLinkPresentation.presentation(for: DeviceRosterItem(id: gatewayID, role: "gateway", revoked: true, keyEpoch: 1), currentDeviceID: nil, status: status(.linked)) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: DeviceRosterItem(id: gatewayID, role: "device", revoked: false, keyEpoch: 1), currentDeviceID: nil, status: status(.linked)) == nil)
        #expect(GoogleMessagesLinkPresentation.presentation(for: remoteGateway, currentDeviceID: gatewayID, status: status(.linked)) == nil)
    }

    @Test func expiresCurrentPresentationAtTheCoreDeadline() {
        let current = GoogleMessagesLinkPresentation.presentation(for: status(.linked, expiresAtMs: 1_001))!
        #expect(current.expired(at: 1_000).status == .connected)
        #expect(current.expired(at: 1_001).status == .unconfirmed)
        #expect(current.expired(at: 1_001).expiresAtMs == nil)
    }

    @Test func discardsAStatusReadAfterLockOrIdentityChange() {
        let request = GoogleMessagesLinkStatusRequest(
            identityKey: "origin|vault|device",
            rosterRevision: 2,
            generation: 4,
            keysUnlocked: true
        )

        #expect(request.accepts(identityKey: "origin|vault|device", rosterRevision: 2, generation: 4, keysUnlocked: true))
        #expect(!request.accepts(identityKey: "origin|vault|device", rosterRevision: 2, generation: 4, keysUnlocked: false))
        #expect(!request.accepts(identityKey: "next-origin|vault|device", rosterRevision: 2, generation: 4, keysUnlocked: true))
        #expect(!request.accepts(identityKey: "origin|vault|device", rosterRevision: 3, generation: 4, keysUnlocked: true))
        #expect(!request.accepts(identityKey: "origin|vault|device", rosterRevision: 2, generation: 5, keysUnlocked: true))
    }

    @Test func readOnlyLinkStatusDoesNotCreateCarrierWork() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let store = InMemorySecureStore()
        let session = NativeSession(
            secureStore: store,
            transport: DeviceScopedTransport(server: server, deviceId: gatewayID),
            databaseDirectory: temporaryDirectory(),
            allowLoopbackHTTP: false
        )
        _ = try await session.importCredential(server.credential(deviceId: gatewayID))
        let pathsBeforeRead = server.paths

        let values = await session.googleMessagesLinkStatuses(
            for: [DeviceRosterItem(id: gatewayID, role: "gateway", revoked: false, keyEpoch: 1)],
            currentDeviceID: nil
        )

        #expect(values.isEmpty)
        #expect(server.paths == pathsBeforeRead)
    }
}
