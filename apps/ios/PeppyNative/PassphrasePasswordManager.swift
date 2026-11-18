import Foundation
import Observation

public enum PassphrasePasswordSaveOutcome: Sendable, Equatable {
    case saved
    case cancelled
    case failed
}

public enum PassphrasePasswordSaveStatus: Sendable, Equatable {
    case idle
    case saving
    case saved
    case failed
}

public protocol PassphrasePasswordSaving: Sendable {
    @MainActor func save(username: String, phrase: String) async -> PassphrasePasswordSaveOutcome
}

@MainActor
@Observable
public final class PassphrasePasswordSaveState {
    public private(set) var status: PassphrasePasswordSaveStatus = .idle
    private var generation = 0

    public init() {}

    public func save(
        username: String,
        phrase: String,
        using saver: some PassphrasePasswordSaving
    ) async {
        guard status != .saving else { return }
        generation &+= 1
        let saveGeneration = generation
        status = .saving

        let outcome = await saver.save(username: username, phrase: phrase)
        guard saveGeneration == generation else { return }

        switch outcome {
        case .saved: status = .saved
        case .cancelled: status = .idle
        case .failed: status = .failed
        }
    }

    public func reset() {
        generation &+= 1
        status = .idle
    }
}

#if os(iOS)
import AuthenticationServices
import Security
import UIKit

@MainActor
public struct SystemPassphrasePasswordSaver: PassphrasePasswordSaving {
    private static let domain = "peppy.pro"

    public init() {}

    public func save(username: String, phrase: String) async -> PassphrasePasswordSaveOutcome {
        guard let anchor = presentationAnchor() else { return .failed }

        if #available(iOS 26.2, *) {
            do {
                try await ASCredentialDataManager().save(
                    password: ASPasswordCredential(user: username, password: phrase),
                    for: ASAutoFillURLScope(host: Self.domain),
                    anchor: anchor
                )
                return .saved
            } catch {
                return Self.outcome(for: error)
            }
        }

        return await withCheckedContinuation { continuation in
            SecAddSharedWebCredential(Self.domain as CFString, username as CFString, phrase as CFString) { error in
                guard let error else {
                    continuation.resume(returning: .saved)
                    return
                }
                continuation.resume(returning: Self.outcome(for: error as Error))
            }
        }
    }

    private func presentationAnchor() -> ASPresentationAnchor? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let activeScenes = scenes.filter { $0.activationState == .foregroundActive }
        return (activeScenes + scenes)
            .lazy
            .flatMap(\.windows)
            .first(where: \.isKeyWindow)
    }

    private nonisolated static func outcome(for error: Error) -> PassphrasePasswordSaveOutcome {
        if let error = error as? ASAuthorizationError, error.code == .canceled {
            return .cancelled
        }
        if (error as NSError).code == Int(errSecUserCanceled) {
            return .cancelled
        }
        return .failed
    }
}
#endif
