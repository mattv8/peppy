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
        phoneResponsive: Bool = true
    ) -> NativeRcsLinkStatus {
        NativeRcsLinkStatus(
            state: state,
            current: current,
            availability: availability,
            health: health,
            phoneResponsive: phoneResponsive,
            observedAtMs: 0,
            capabilityRevision: 1,
            featureBits: 0
        )
    }

    @Test func mapsMissingAndUnlinkedStatusesToNoPresentation() {
        #expect(GoogleMessagesLinkStatus.presentation(for: nil) == nil)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.unlinked)) == nil)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.unavailable)) == nil)
    }

    @Test func mapsCurrentStatusesAndAttentionHelp() {
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.linked)) == .connected)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.linked, phoneResponsive: false)) == .phoneNotResponding)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.needsRelink)) == .needsRelink)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.signedOut)) == .signedOut)
        #expect(GoogleMessagesLinkStatus.needsRelink.helpKey == "google_messages_link_remote_help_attention")
        #expect(GoogleMessagesLinkStatus.phoneNotResponding.helpKey == "google_messages_link_remote_help_unresponsive")
    }

    @Test func mapsStaleEvidenceToUnconfirmedBeforeLinkHealth() {
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.linked, current: false)) == .unconfirmed)
        #expect(GoogleMessagesLinkStatus.presentation(for: status(.needsRelink, current: false)) == .unconfirmed)
    }

    @Test func filtersRevokedCurrentAndNonGatewayRowsWithoutCallingCore() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let session = NativeSession(secureStore: InMemorySecureStore(), transport: server, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        let rows = [
            DeviceRosterItem(id: gatewayID, role: "gateway", revoked: true, keyEpoch: 1),
            DeviceRosterItem(id: "00000000-0000-0000-0000-000000000002", role: "device", revoked: false, keyEpoch: 1),
            DeviceRosterItem(id: "00000000-0000-0000-0000-000000000003", role: "gateway", revoked: false, keyEpoch: 1),
        ]

        let values = await session.googleMessagesLinkStatuses(for: rows, currentDeviceID: "00000000-0000-0000-0000-000000000003")

        #expect(values.isEmpty)
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
