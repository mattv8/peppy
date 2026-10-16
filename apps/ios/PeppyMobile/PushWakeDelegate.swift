import Foundation
#if canImport(UIKit)
import UIKit
import UserNotifications
#if canImport(PeppyNative)
import PeppyNative
#endif

/// App-owned wiring for bounded APNs wake hints. It does not create a socket or promise execution.
@MainActor
final class PushWakeDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    private static weak var model: AppModel?

    static func install(model: AppModel) {
        self.model = model
    }

    func registerForRemoteNotifications() {
        UIApplication.shared.registerForRemoteNotifications()
    }

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        Task { @MainActor in await Self.model?.reconcileWakeRegistration() }
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken token: Data) {
        let value = token.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in await Self.model?.receiveAPNSToken(value) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        Self.model?.apnsRegistrationFailed()
    }

    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any], fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void) {
        let callback = BackgroundFetchCallback(completionHandler)
        let completion = OnceCompletion { callback.call($0 ? .newData : .failed) }
        guard Self.model?.backgroundUpdatesEnabled == true,
              let peppy = userInfo["peppy"] as? [AnyHashable: Any],
              let kind = peppy["kind"] as? String else { completion.complete(false); return }
        let cancellation = BackgroundWakeCancellation()
        let background = application.beginBackgroundTask(withName: "peppy-push-wake") {
            Task { @MainActor in
                cancellation.expire()
                completion.complete(false)
            }
        }
        let task = Task { @MainActor [weak application] in
            defer { if background != .invalid { application?.endBackgroundTask(background) } }
            guard !Task.isCancelled else { return }
            let model = Self.model
            switch kind {
            case "challenge":
                let value = peppy["value"] as? String ?? ""
                // A cold APNs launch runs before SwiftUI's startup task. Restore the native
                // identity/runtime first so an otherwise valid challenge is not discarded.
                await model?.reconcileWakeRegistration()
                completion.complete((try? await model?.relay?.confirm(challenge: value)) ?? false)
            case "wake":
                // Both passes are bounded by their native coordinators; cancellation propagates on expiry.
                let synced = await model?.wakeSync() ?? false
                let contactsDone = await model?.wakeContacts() ?? true
                completion.complete(synced || contactsDone)
            default: completion.complete(false)
            }
        }
        cancellation.install(task)
    }
}

/// UIKit's completion closure is not `Sendable`; keep it behind this one-shot-safe wrapper before
/// handing it to the sendable completion coordinator used by the background worker.
private final class BackgroundFetchCallback: @unchecked Sendable {
    private let handler: (UIBackgroundFetchResult) -> Void
    init(_ handler: @escaping (UIBackgroundFetchResult) -> Void) { self.handler = handler }
    func call(_ result: UIBackgroundFetchResult) { handler(result) }
}
#endif
