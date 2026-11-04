package dev.peppy.mobile.replyviagm

import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Build
import android.service.notification.StatusBarNotification
import android.telephony.TelephonyManager
import androidx.core.app.NotificationCompat
import androidx.core.app.RemoteInput
import dev.peppy.mobile.NotificationMirrorAccess
import dev.peppy.mobile.NotificationMirrorService
import dev.peppy.mobile.sms.DispatchSummary
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSendResult

internal fun interface ReplyViaGmSettings { suspend fun enabled(): Boolean }

internal fun interface ReplyViaGmSubmissionRecorder {
    fun record(commandId: String, submittedAtMs: Long)
}

internal fun interface ReplyViaGmPhoneCanonicalizer { fun e164(address: String): String? }

internal object ReplyViaGmDiagnostics {
    data class Counts(var eligible: Int = 0, var fired: Int = 0, var failed: Int = 0, val ineligible: MutableMap<String, Int> = mutableMapOf())
    @Volatile private var counts = Counts()
    fun ineligible(reason: String) { synchronized(this) { counts.ineligible[reason] = (counts.ineligible[reason] ?: 0) + 1 } }
    fun eligible() { synchronized(this) { counts.eligible++ } }
    fun fired() { synchronized(this) { counts.fired++ } }
    fun failed() { synchronized(this) { counts.failed++ } }
}

internal data class ReplyCandidate(val key: String, val postTime: Long, val actionIndex: Int)

internal data class PreparedReply(val actionIntent: PendingIntent, val remoteInput: RemoteInput)

internal object ReplyViaGmMatcher {
    private const val GOOGLE_MESSAGES = "com.google.android.apps.messaging"
    private const val RCS_ID = "extra_im_notification_latest_rcs_message_id"
    private const val DESTINATION = "extra_im_notification_participant_normalized_destination"

    fun match(recipient: String, notifications: Array<StatusBarNotification>, canonicalizer: ReplyViaGmPhoneCanonicalizer): ReplyCandidate? {
        val normalizedRecipient = canonicalizer.e164(recipient) ?: return null
        val candidates = notifications.mapNotNull { notification -> candidate(notification, normalizedRecipient, canonicalizer) }
        return candidates.singleOrNull()
    }

    private fun candidate(sbn: StatusBarNotification, recipient: String, canonicalizer: ReplyViaGmPhoneCanonicalizer): ReplyCandidate? {
        if (sbn.packageName != GOOGLE_MESSAGES) return null
        val extras = sbn.notification.extras ?: return null
        if (!extras.containsKey(Notification.EXTRA_IS_GROUP_CONVERSATION) || extras.getBoolean(Notification.EXTRA_IS_GROUP_CONVERSATION, true) ||
            sbn.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 ||
            NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(sbn.notification)?.isGroupConversation == true ||
            extras.getString(RCS_ID) == null) return null
        if (canonicalizer.e164(extras.getString(DESTINATION) ?: return null) != recipient) return null
        val actions = (0 until NotificationCompat.getActionCount(sbn.notification)).mapNotNull { index ->
            NotificationCompat.getAction(sbn.notification, index)?.let { index to it }
        }
        val reply = actions.filter { (_, action) -> action.semanticAction == NotificationCompat.Action.SEMANTIC_ACTION_REPLY }
        val selected = when {
            reply.size == 1 -> reply.single()
            reply.isNotEmpty() -> return null
            else -> actions.filter { (_, action) -> action.semanticAction == NotificationCompat.Action.SEMANTIC_ACTION_NONE && action.remoteInputs?.isNotEmpty() == true }.singleOrNull()
                ?: return null
        }
        val remoteInputs = selected.second.remoteInputs?.filter { it.allowFreeFormInput }.orEmpty()
        if (selected.second.actionIntent == null || remoteInputs.size != 1) return null
        return ReplyCandidate(sbn.key, sbn.postTime, selected.first)
    }

    fun current(candidate: ReplyCandidate, recipient: String, notifications: Array<StatusBarNotification>, canonicalizer: ReplyViaGmPhoneCanonicalizer): Pair<StatusBarNotification, ReplyCandidate>? {
        val notification = notifications.firstOrNull { it.key == candidate.key && it.postTime == candidate.postTime } ?: return null
        val rematch = match(recipient, arrayOf(notification), canonicalizer) ?: return null
        if (rematch.actionIndex != candidate.actionIndex) return null
        return notification to rematch
    }

    fun prepare(notification: StatusBarNotification, candidate: ReplyCandidate): PreparedReply? {
        val action = NotificationCompat.getAction(notification.notification, candidate.actionIndex) ?: return null
        val remoteInputs = action.remoteInputs?.filter { it.allowFreeFormInput }.orEmpty()
        val actionIntent = action.actionIntent ?: return null
        return if (remoteInputs.size == 1) PreparedReply(actionIntent, remoteInputs.single()) else null
    }

}

internal class ReplyViaGmDispatcher(
    private val context: Context,
    private val client: NativeClientInterface,
    private val settings: ReplyViaGmSettings = ReplyViaGmSettings { ReplyViaGmPreferences(context).enabled() },
    private val recorder: ReplyViaGmSubmissionRecorder = ReplyViaGmSubmissionRecorder { commandId, submittedAtMs ->
        client.recordNotificationReplySubmission(commandId, submittedAtMs)
    },
    private val accessGranted: () -> Boolean = { NotificationMirrorAccess.granted(context) },
    private val listenerConnected: () -> Boolean = NotificationMirrorService::isConnected,
    private val notifications: () -> Array<StatusBarNotification> = NotificationMirrorService::currentNotifications,
    private val sender: (PendingIntent, Intent) -> Unit = { intent, fillIn -> intent.send(context, 0, fillIn) },
    private val routes: List<SimRoute> = emptyList(),
    private val canonicalizer: ReplyViaGmPhoneCanonicalizer = ReplyViaGmPhoneCanonicalizer { address -> client.canonicalPhoneE164(address) },
    private val activeModemCount: () -> Int? = {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) context.getSystemService(TelephonyManager::class.java)?.activeModemCount else null
    },
) {
    fun dispatch(limit: Int): DispatchSummary {
        if (!enabled() || !accessGranted() || !listenerConnected()) {
            ReplyViaGmDiagnostics.ineligible("unavailable")
            return DispatchSummary(0, 0, 0, false)
        }
        var submitted = 0
        var refused = 0
        var permits = 0
        var more = false
        val active = notifications()
        for (pending in client.pendingCommands()) {
            if (permits >= limit) { more = true; break }
            val message = pending.message
            if (message.body.isEmpty() || message.subject != null || message.attachmentIds.isNotEmpty() || message.recipients.size != 1) {
                ReplyViaGmDiagnostics.ineligible("shape")
                continue
            }
            val route = SimRoutes.resolve(pending.subscriptionId, routes)
            if (route == null || activeModemCount() != 1) {
                ReplyViaGmDiagnostics.ineligible("route")
                continue
            }
            val candidate = ReplyViaGmMatcher.match(message.recipients.single(), active, canonicalizer)
            if (candidate == null) {
                ReplyViaGmDiagnostics.ineligible("notification")
                continue
            }
            val fresh = ReplyViaGmMatcher.current(candidate, message.recipients.single(), notifications(), canonicalizer)
            if (fresh == null) {
                ReplyViaGmDiagnostics.ineligible("stale")
                continue
            }
            val prepared = ReplyViaGmMatcher.prepare(fresh.first, fresh.second)
            if (prepared == null) {
                ReplyViaGmDiagnostics.ineligible("action")
                continue
            }
            val permit = client.beginSendAttempt(pending.commandId)
            if (permit.state != NativePermitState.PERMIT || permit.command?.commandId != pending.commandId) continue
            permits++
            ReplyViaGmDiagnostics.eligible()
            try {
                recorder.record(pending.commandId, System.currentTimeMillis())
            } catch (_: Exception) {
                fail(pending.commandId)
                ReplyViaGmDiagnostics.failed()
                refused++
                continue
            }
            try {
                val fillIn = Intent()
                RemoteInput.addResultsToIntent(arrayOf(prepared.remoteInput), fillIn, Bundle().apply { putCharSequence(prepared.remoteInput.resultKey, message.body) })
                RemoteInput.setResultsSource(fillIn, RemoteInput.SOURCE_FREE_FORM_INPUT)
                val current = ReplyViaGmMatcher.current(candidate, message.recipients.single(), notifications(), canonicalizer)
                if (current == null || ReplyViaGmMatcher.prepare(current.first, current.second) == null) {
                    fail(pending.commandId); ReplyViaGmDiagnostics.failed(); refused++
                    continue
                }
                sender(prepared.actionIntent, fillIn)
                record(pending.commandId, NativeSendResult.SUBMITTED)
                ReplyViaGmDiagnostics.fired()
                submitted++
            } catch (_: PendingIntent.CanceledException) {
                fail(pending.commandId); ReplyViaGmDiagnostics.failed(); refused++
            } catch (_: Exception) { }
        }
        return DispatchSummary(submitted, refused, 0, more)
    }

    private fun enabled(): Boolean = try { kotlinx.coroutines.runBlocking { settings.enabled() } } catch (_: Exception) { false }
    private fun fail(commandId: String) { try { client.recordSendResult(commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION) } catch (_: MobileBindingsException) {} }
    private fun record(commandId: String, result: NativeSendResult) { try { client.recordSendResult(commandId, result) } catch (_: MobileBindingsException) {} }
}
