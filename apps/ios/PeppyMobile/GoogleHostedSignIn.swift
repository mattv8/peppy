import GoogleSignIn
import UIKit

/// Bridges a nonce-bound Google SDK result to the server-owned hosted account session.
@MainActor
final class GoogleHostedSignIn {
    enum Error: Swift.Error, Equatable { case unconfigured, unavailable, cancelled, missingIDToken }

    private struct Configuration { let clientID: String; let serverClientID: String; let reversedClientID: String }

    static func isAvailable() -> Bool { configuration != nil }

    func signIn(client: HostedAccountClient, presenting: UIViewController) async throws -> NativeHostedAccount {
        guard let configuration = Self.configuration else { throw Error.unconfigured }
        guard try await client.availableProviders().contains("google") else { throw Error.unavailable }
        let attempt = try await client.beginGoogleSignIn()
        GIDSignIn.sharedInstance.configuration = GIDConfiguration(clientID: configuration.clientID, serverClientID: configuration.serverClientID)
        do {
            let result = try await GIDSignIn.sharedInstance.signIn(withPresenting: presenting, hint: nil, additionalScopes: [], nonce: attempt.nonce())
            guard let idToken = result.user.idToken?.tokenString, !idToken.isEmpty else { throw Error.missingIDToken }
            return try await client.finishGoogleSignIn(attemptID: attempt.attemptId(), idToken: idToken)
        } catch let error as Error {
            throw error
        } catch let error as NSError where error.domain == kGIDSignInErrorDomain && error.code == GIDSignInError.canceled.rawValue {
            throw Error.cancelled
        }
    }

    static func handle(_ url: URL) -> Bool {
        guard let configuration = Self.configuration, url.scheme == configuration.reversedClientID else { return false }
        return GIDSignIn.sharedInstance.handle(url)
    }

    private static var configuration: Configuration? {
        guard let clientID = configuredValue("PEPPY_GOOGLE_IOS_CLIENT_ID"), let serverClientID = configuredValue("PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID"), let reversedClientID = configuredValue("PEPPY_GOOGLE_REVERSED_CLIENT_ID"), isGoogleClientID(clientID), isGoogleClientID(serverClientID), reversedClientID == reversedGoogleClientID(for: clientID) else { return nil }
        return Configuration(clientID: clientID, serverClientID: serverClientID, reversedClientID: reversedClientID)
    }

    private static func configuredValue(_ key: String) -> String? {
        guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !trimmed.contains("$(") else { return nil }
        return trimmed
    }

    private static func isGoogleClientID(_ value: String) -> Bool {
        let parts = value.split(separator: "-", maxSplits: 1, omittingEmptySubsequences: false)
        return parts.count == 2 && !parts[0].isEmpty && !parts[1].isEmpty && value.hasSuffix(".apps.googleusercontent.com")
    }

    private static func reversedGoogleClientID(for clientID: String) -> String {
        "com.googleusercontent.apps." + String(clientID.dropLast(".apps.googleusercontent.com".count))
    }
}
