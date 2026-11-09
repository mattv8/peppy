import Foundation
import Testing
@testable import PeppyNative

@Suite("Mitigation recovery")
struct MitigationRecoveryTests {
    @Test func selfDisconnectTreatsThrownUnauthorizedAsTerminalAndArchivesOnlyTheCredential() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let store = InMemorySecureStore()
        let device = UUID().uuidString.lowercased()
        let identity = EnrollmentIdentity(origin: FakeServer.origin, vaultId: server.vaultId, deviceId: device)
        let session = NativeSession(secureStore: store, transport: DeviceScopedTransport(server: server, deviceId: device), databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.importCredential(server.credential(deviceId: device))
        _ = try await session.unlock(passphrase: "io synthetic vault passphrase")
        server.mutate { server in
            server.override = { request in
                request.url.path.hasSuffix("/revoke") ? server.reply(request, 401, ["code": "unauthorized"]) : nil
            }
        }

        let status = try await session.disconnect()

        #expect(!status.databaseOpen)
        #expect(try EnrollmentStore(secure: store).activeIdentity() == nil)
        #expect(try EnrollmentStore(secure: store).record(identity).identity == identity)
        #expect(try EnrollmentStore(secure: store).token(identity) == server.token)
    }

    @Test func otherDeviceUnauthorizedLeavesTheActiveEnrollmentUntouched() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let store = InMemorySecureStore()
        let device = UUID().uuidString.lowercased()
        let identity = EnrollmentIdentity(origin: FakeServer.origin, vaultId: server.vaultId, deviceId: device)
        let otherDevice = UUID().uuidString.lowercased()
        let session = NativeSession(secureStore: store, transport: DeviceScopedTransport(server: server, deviceId: device), databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.importCredential(server.credential(deviceId: device))
        _ = try await session.unlock(passphrase: "io synthetic vault passphrase")
        server.mutate { server in
            server.override = { request in
                request.url.path.hasSuffix("/revoke") ? server.reply(request, 401, ["code": "unauthorized"]) : nil
            }
        }

        await #expect(throws: ClientError.unauthorized) {
            try await session.revokeDevice(otherDevice)
        }

        #expect(try EnrollmentStore(secure: store).activeIdentity() == identity)
        #expect(try await session.open().databaseOpen)
    }

    @Test func selfRevokeAndDisconnectRejectForbiddenWithoutArchiving() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let store = InMemorySecureStore()
        let device = UUID().uuidString.lowercased()
        let identity = EnrollmentIdentity(origin: FakeServer.origin, vaultId: server.vaultId, deviceId: device)
        let transport = RevokeFailureTransport(inner: DeviceScopedTransport(server: server, deviceId: device))
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.importCredential(server.credential(deviceId: device))
        _ = try await session.unlock(passphrase: "io synthetic vault passphrase")
        transport.failure = .forbidden

        await #expect(throws: ClientError.forbidden) { try await session.revokeDevice(device) }
        await #expect(throws: ClientError.forbidden) { try await session.disconnect() }
        #expect(try EnrollmentStore(secure: store).activeIdentity() == identity)
        #expect(try await session.open().databaseOpen)
    }

    @Test func selfRevokeAndDisconnectRejectNetworkFailureWithoutArchiving() async throws {
        let server = try FakeServer(passphrase: "io synthetic vault passphrase")
        let store = InMemorySecureStore()
        let device = UUID().uuidString.lowercased()
        let identity = EnrollmentIdentity(origin: FakeServer.origin, vaultId: server.vaultId, deviceId: device)
        let transport = RevokeFailureTransport(inner: DeviceScopedTransport(server: server, deviceId: device))
        let session = NativeSession(secureStore: store, transport: transport, databaseDirectory: temporaryDirectory(), allowLoopbackHTTP: false)
        _ = try await session.importCredential(server.credential(deviceId: device))
        _ = try await session.unlock(passphrase: "io synthetic vault passphrase")
        transport.failure = .network(code: -1009)

        await #expect(throws: ClientError.network(code: -1009)) { try await session.revokeDevice(device) }
        await #expect(throws: ClientError.network(code: -1009)) { try await session.disconnect() }
        #expect(try EnrollmentStore(secure: store).activeIdentity() == identity)
        #expect(try await session.open().databaseOpen)
    }

}

private final class RevokeFailureTransport: HTTPTransport, @unchecked Sendable {
    private let inner: any HTTPTransport
    private let lock = NSLock()
    private var configuredFailure: ClientError?

    var failure: ClientError? {
        get { lock.withLock { configuredFailure } }
        set { lock.withLock { configuredFailure = newValue } }
    }

    init(inner: any HTTPTransport) { self.inner = inner }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        if request.url.path.hasSuffix("/revoke"), let failure { throw failure }
        return try await inner.send(request)
    }
}
