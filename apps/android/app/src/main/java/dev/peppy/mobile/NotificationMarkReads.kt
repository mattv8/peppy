package dev.peppy.mobile

import android.app.KeyguardManager
import android.app.PendingIntent
import android.service.notification.StatusBarNotification
import dev.peppy.mobile.diagnostics.NotificationMarkReadDiagnostics
import dev.peppy.mobile.phoneread.PhoneReadMatcher
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeNotificationMarkRead

/** Core-owned mark-read requests: pending lists only, and begin consumes one request. */
internal interface NotificationMarkReadCore {
    fun pending(nowMs: Long): List<NativeNotificationMarkRead>
    fun begin(id: String, nowMs: Long): Boolean
}

internal interface NotificationMarkReadNotifications {
    /** Null when the OS cannot report active notifications; nothing is sent then. */
    fun active(): List<StatusBarNotification>?
    fun deviceUnlocked(): Boolean
}

/**
 * Presses an app's own single safe Mark as read action for a mirrored notification. Each request
 * is consumed before its action is sent and is never retried. A request whose notification has no
 * safe action is consumed without touching the notification: mark-read never dismisses.
 */
internal class NotificationMarkReadEffects(
    private val core: NotificationMarkReadCore,
    private val notifications: NotificationMarkReadNotifications,
    private val permitted: () -> Boolean,
    private val send: (PendingIntent) -> Unit = { it.send() },
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val counters: NotificationMarkReadDiagnostics.Counters = NotificationMarkReadDiagnostics.Counters(),
) {
    /** Returns how many actions were sent. Any OS or core failure stops the batch. */
    fun run(): Int = drain().also { NotificationMarkReadDiagnostics.pass(counters) }

    private fun drain(): Int {
        val requests = try {
            core.pending(nowMs())
        } catch (_: MobileBindingsException) {
            counters.failed++
            return 0
        }
        var sent = 0
        for (request in requests) {
            if (!permitted()) return sent
            counters.attempted++
            try {
                if (attempt(request)) {
                    sent++
                    counters.sent++
                }
            } catch (_: SecurityException) {
                counters.failed++
                return sent
            } catch (_: MobileBindingsException) {
                counters.failed++
                return sent
            }
        }
        return sent
    }

    private fun attempt(request: NativeNotificationMarkRead): Boolean {
        // Stale or absent instances stay pending; a repost with the same key is a different request.
        val current = currentInstance(request) ?: return false
        val action = PhoneReadMatcher.markReadAction(PhoneReadMatcher.facts(current))
        if (action == null) {
            core.begin(request.id, nowMs())
            return false
        }
        if (action.requiresUnlockedDevice && !notifications.deviceUnlocked()) return false
        if (!permitted()) return false
        // The one-use permit is consumed here. Anything after this point is terminal for the request.
        if (!core.begin(request.id, nowMs())) return false
        val fresh = currentInstance(request) ?: return false
        val freshAction = PhoneReadMatcher.markReadAction(PhoneReadMatcher.facts(fresh)) ?: return false
        if (freshAction.requiresUnlockedDevice && !notifications.deviceUnlocked()) return false
        val intent = PhoneReadMatcher.actionAt(fresh.notification, freshAction.source, freshAction.index)?.actionIntent ?: return false
        return try {
            send(intent)
            true
        } catch (_: PendingIntent.CanceledException) {
            // The consumed permit is terminal even when Android cancels the action.
            counters.canceled++
            false
        }
    }

    private fun currentInstance(request: NativeNotificationMarkRead): StatusBarNotification? =
        notifications.active()?.firstOrNull {
            it.key == request.target.notificationKey && "${it.key}:${it.postTime}" == request.instance
        }
}

internal class NativeNotificationMarkReadCore(private val client: NativeClientInterface) : NotificationMarkReadCore {
    override fun pending(nowMs: Long) = client.pendingNotificationMarkReads(nowMs, 50uL)
    override fun begin(id: String, nowMs: Long) = client.beginNotificationMarkRead(id, nowMs)
}

internal class AndroidNotificationMarkReadNotifications(private val service: NotificationMirrorService) : NotificationMarkReadNotifications {
    override fun active(): List<StatusBarNotification>? = service.currentNotifications()?.toList()
    override fun deviceUnlocked(): Boolean = service.getSystemService(KeyguardManager::class.java)?.isDeviceLocked == false
}
