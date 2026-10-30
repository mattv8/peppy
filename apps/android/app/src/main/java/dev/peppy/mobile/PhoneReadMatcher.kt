package dev.peppy.mobile

import android.app.Notification
import android.os.Build
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat

internal enum class ActionSource { VISIBLE, INVISIBLE }

internal data class ReadActionFacts(
    val source: ActionSource,
    val index: Int,
    val markAsRead: Boolean,
    val remoteInputCount: Int,
    val showsUserInterface: Boolean,
    /** API-level intent/authentication constraints; retained so semantic uniqueness stays strict. */
    val actionPermitted: Boolean = true,
    /** Invisible Android 12+ actions lose authentication metadata during compat conversion. */
    val requiresUnlockedDevice: Boolean = false,
)

/** Deliberately excludes notification content from its string representation. */
internal data class ConversationNotification(
    val key: String,
    val postTime: Long,
    val groupSummary: Boolean,
    val messageTimes: Set<Long>,
    val actions: List<ReadActionFacts>,
) {
    override fun toString() = "ConversationNotification(postTime=$postTime, messages=${messageTimes.size}, actions=${actions.size})"
}

internal data class ProviderThread(val threadId: Long, val unreadDates: Set<Long>, val rowDates: Set<Long>)

internal sealed interface ReadMatch {
    data class Ready(val notification: ConversationNotification, val action: ReadActionFacts) : ReadMatch
    data object NoCandidate : ReadMatch
    data object Ambiguous : ReadMatch
    data object NoAction : ReadMatch
}

internal object PhoneReadMatcher {
    fun match(thread: ProviderThread, notifications: List<ConversationNotification>): ReadMatch {
        if (thread.unreadDates.isEmpty()) return ReadMatch.NoCandidate
        val candidates = notifications.filter {
            !it.groupSummary && it.messageTimes.isNotEmpty() &&
                it.messageTimes.containsAll(thread.unreadDates) && thread.rowDates.containsAll(it.messageTimes)
        }
        val notification = when (candidates.size) {
            0 -> return ReadMatch.NoCandidate
            1 -> candidates.single()
            else -> return ReadMatch.Ambiguous
        }
        return markReadAction(notification)?.let { ReadMatch.Ready(notification, it) } ?: ReadMatch.NoAction
    }

    fun markReadAction(notification: ConversationNotification): ReadActionFacts? {
        val semantic = notification.actions.filter { it.markAsRead }
        return semantic.singleOrNull()?.takeIf {
            it.actionPermitted && it.remoteInputCount == 0 && !it.showsUserInterface
        }
    }

    fun facts(sbn: StatusBarNotification): ConversationNotification {
        val notification = sbn.notification
        val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(notification)
        val messageTimes = style?.messages.orEmpty().map { it.timestamp }.toSet()
        return ConversationNotification(
            key = sbn.key,
            postTime = sbn.postTime,
            groupSummary = notification.flags and Notification.FLAG_GROUP_SUMMARY != 0,
            messageTimes = messageTimes,
            actions = visibleActions(notification) + invisibleActions(notification),
        )
    }

    fun actionAt(notification: Notification, source: ActionSource, index: Int): NotificationCompat.Action? = when (source) {
        ActionSource.VISIBLE -> NotificationCompat.getAction(notification, index)
        ActionSource.INVISIBLE -> NotificationCompat.getInvisibleActions(notification).getOrNull(index)
    }

    private fun visibleActions(notification: Notification) = (0 until NotificationCompat.getActionCount(notification)).mapNotNull { index ->
        NotificationCompat.getAction(notification, index)?.let { action -> actionFacts(notification, index, ActionSource.VISIBLE, action) }
    }

    private fun invisibleActions(notification: Notification) = NotificationCompat.getInvisibleActions(notification).mapIndexedNotNull { index, action ->
        actionFacts(notification, index, ActionSource.INVISIBLE, action)
    }

    private fun actionFacts(notification: Notification, index: Int, source: ActionSource, action: NotificationCompat.Action): ReadActionFacts? {
        val intent = action.actionIntent
        val actionPermitted = intent != null && (Build.VERSION.SDK_INT < Build.VERSION_CODES.S ||
            (!intent.isActivity && (source == ActionSource.INVISIBLE || !action.isAuthenticationRequired)))
        return ReadActionFacts(
            source, index,
            action.semanticAction == NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ,
            action.remoteInputs?.size ?: 0,
            action.showsUserInterface,
            actionPermitted,
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && source == ActionSource.INVISIBLE,
        )
    }
}
