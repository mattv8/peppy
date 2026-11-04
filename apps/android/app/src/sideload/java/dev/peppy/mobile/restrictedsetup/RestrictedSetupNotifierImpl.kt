package dev.peppy.mobile.restrictedsetup

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import dev.peppy.mobile.MainActivity
import dev.peppy.mobile.R

internal class RestrictedSetupNotifierImpl(private val context: Context) : RestrictedSetupNotifier {
    override fun canPrompt(): Boolean {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) return false
        if (!NotificationManagerCompat.from(context).areNotificationsEnabled()) return false
        return Build.VERSION.SDK_INT < 26 || manager.getNotificationChannel(CHANNEL_ID)?.importance != NotificationManager.IMPORTANCE_NONE
    }

    override fun show(notice: RestrictedSetupNotice) {
        ensureChannel()
        try {
            NotificationManagerCompat.from(context).notify(notificationId(notice), notification(notice))
        } catch (_: SecurityException) {
            // Notification availability can change between canPrompt and notify.
        }
    }

    override fun clear() {
        NotificationManagerCompat.from(context).apply {
            cancel(NOTIFICATION_ID)
            cancel(RESULT_NOTIFICATION_ID)
        }
    }

    fun ongoing(): Notification {
        ensureChannel()
        return notification(RestrictedSetupNotice.SEARCHING)
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < 26 || manager.getNotificationChannel(CHANNEL_ID) != null) return
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, context.getString(R.string.peppy_restricted_setup_notif_channel_name), NotificationManager.IMPORTANCE_HIGH).apply {
                setSound(null, null)
                enableVibration(false)
            },
        )
    }

    private fun notification(notice: RestrictedSetupNotice): Notification {
        val isPrompt = notice in setOf(RestrictedSetupNotice.CODE_PROMPT, RestrictedSetupNotice.CODE_INVALID, RestrictedSetupNotice.CODE_REJECTED)
        val acceptsReply = notice != RestrictedSetupNotice.CODE_REJECTED && isPrompt
        val hasCancelAction = isPrompt || notice in setOf(RestrictedSetupNotice.SEARCHING, RestrictedSetupNotice.NETWORK_NOT_ALLOWED)
        val isResult = notice in setOf(RestrictedSetupNotice.DONE, RestrictedSetupNotice.FAILED)
        val text = when (notice) {
            RestrictedSetupNotice.SEARCHING -> context.getString(R.string.peppy_restricted_setup_discovery_searching)
            RestrictedSetupNotice.NETWORK_NOT_ALLOWED -> context.getString(R.string.peppy_restricted_setup_notif_network_not_allowed)
            RestrictedSetupNotice.CODE_PROMPT -> context.getString(R.string.peppy_restricted_setup_notif_prompt_text)
            RestrictedSetupNotice.CODE_INVALID -> context.getString(R.string.peppy_restricted_setup_notif_code_invalid)
            RestrictedSetupNotice.CODE_REJECTED -> context.getString(R.string.peppy_restricted_setup_notif_code_rejected)
            RestrictedSetupNotice.WORKING -> context.getString(R.string.peppy_restricted_setup_notif_working)
            RestrictedSetupNotice.DONE -> context.getString(R.string.peppy_restricted_setup_notif_done)
            RestrictedSetupNotice.FAILED -> context.getString(R.string.peppy_restricted_setup_notif_failed)
        }
        return NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher_monochrome)
            .setContentTitle(context.getString(R.string.peppy_restricted_setup_notif_prompt_title))
            .setContentText(text)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(publicNotification())
            .setOnlyAlertOnce(notice != RestrictedSetupNotice.CODE_PROMPT)
            .setOngoing(!isResult)
            .setAutoCancel(isResult)
            .setTimeoutAfter(if (isResult) RESULT_TIMEOUT_MILLIS else PROMPT_TIMEOUT_MILLIS)
            .setContentIntent(if (isResult) openAppIntent() else null)
            .apply {
                if (notice == RestrictedSetupNotice.WORKING) setProgress(0, 0, true)
                if (acceptsReply) addAction(replyAction(notice))
                if (hasCancelAction) addAction(cancelAction())
            }
            .build()
    }

    private fun publicNotification(): Notification = NotificationCompat.Builder(context, CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_launcher_monochrome)
        .setContentTitle(context.getString(R.string.peppy_restricted_setup_notif_prompt_title))
        .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        .build()

    private fun replyAction(notice: RestrictedSetupNotice): NotificationCompat.Action {
        val remoteInput = RemoteInput.Builder(RestrictedSetupReplyReceiver.RESULT_KEY)
            .setLabel(context.getString(R.string.peppy_restricted_setup_notif_prompt_label))
            .setAllowFreeFormInput(true)
            .build()
        return NotificationCompat.Action.Builder(
            0,
            context.getString(R.string.peppy_restricted_setup_notif_prompt_action),
            receiverIntent(RestrictedSetupReplyReceiver.ACTION_REPLY, mutable = true, notice = notice),
        ).addRemoteInput(remoteInput).setAllowGeneratedReplies(false).build()
    }

    private fun cancelAction(): NotificationCompat.Action = NotificationCompat.Action.Builder(
        0,
        context.getString(R.string.peppy_restricted_setup_notif_cancel_action),
        receiverIntent(RestrictedSetupReplyReceiver.ACTION_CANCEL, mutable = false),
    ).build()

    private fun receiverIntent(action: String, mutable: Boolean, notice: RestrictedSetupNotice? = null): PendingIntent = PendingIntent.getBroadcast(
        context,
        action.hashCode(),
        Intent(context, RestrictedSetupReplyReceiver::class.java).setAction(action).setPackage(context.packageName).apply {
            notice?.let { putExtra(RestrictedSetupReplyReceiver.EXTRA_NOTICE, it.name) }
        },
        PendingIntent.FLAG_UPDATE_CURRENT or if (mutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE,
    )

    private fun openAppIntent(): PendingIntent = PendingIntent.getActivity(
        context,
        NOTIFICATION_ID,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private val manager: NotificationManager
        get() = context.getSystemService(NotificationManager::class.java)

    private fun notificationId(notice: RestrictedSetupNotice): Int =
        if (notice in setOf(RestrictedSetupNotice.DONE, RestrictedSetupNotice.FAILED)) RESULT_NOTIFICATION_ID else NOTIFICATION_ID

    internal companion object {
        const val CHANNEL_ID = "restricted_setup"
        const val NOTIFICATION_ID = 4108
        const val RESULT_NOTIFICATION_ID = 4109
        private const val PROMPT_TIMEOUT_MILLIS = 10 * 60_000L
        private const val RESULT_TIMEOUT_MILLIS = 5 * 60_000L
    }
}
