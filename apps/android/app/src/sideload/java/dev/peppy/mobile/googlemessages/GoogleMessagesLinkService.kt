package dev.peppy.mobile.googlemessages

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.peppy.mobile.MainActivity
import dev.peppy.mobile.R
import java.util.concurrent.atomic.AtomicBoolean

class GoogleMessagesLinkService : Service() {
    private val runStarted = AtomicBoolean(false)
    private val lifecycleLock = Any()
    private val backoff = RunBackoff()
    private var lastStartId = 0

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForegroundFor(LinkStatus(0, "pairing", false, false, true, 0, ""))
        synchronized(lifecycleLock) {
            lastStartId = startId
            if (runStarted.compareAndSet(false, true)) startRunThread()
        }
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startRunThread() {
        Thread {
            try {
                waitForLinkedRun()
            } finally {
                synchronized(lifecycleLock) {
                    runStarted.set(false)
                    stopSelf(lastStartId)
                }
            }
        }.start()
    }

    private fun waitForLinkedRun() {
        while (true) {
            val opened = LinkRuntime.handle(this)
            val handle = (opened as? LinkOpenResult.Open)?.handle ?: return
            val status = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            startForegroundFor(status)
            when (nextServiceAction(status, LinkRuntime.pairingInProgress.get(), status.running)) {
                ServiceAction.STOP -> {
                    showAttentionIfNeeded(status)
                    return
                }
                ServiceAction.WAIT -> {
                    Thread.sleep(PAIRING_WAIT_MS)
                    continue
                }
                ServiceAction.RUN -> runWithRetry(handle)
            }
        }
    }

    private fun runWithRetry(handle: LinkHandle) {
        while (true) {
            val beforeRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (beforeRun.state != "linked") return
            startForegroundFor(beforeRun)
            val failure = runCatching { handle.run() }.exceptionOrNull()
            val afterRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            startForegroundFor(afterRun)
            if (afterRun.state != "linked") {
                showAttentionIfNeeded(afterRun)
                return
            }
            if (failure == null) return
            Thread.sleep(backoff.nextDelayMillis())
        }
    }

    private fun startForegroundFor(status: LinkStatus) {
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(status), foregroundServiceType())
    }

    private fun showAttentionIfNeeded(status: LinkStatus) {
        if (status.state !in ATTENTION_STATES) return
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_DETACH)
        NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification(status, attention = true))
    }

    private fun notification(status: LinkStatus, attention: Boolean = false): Notification {
        val text = when {
            attention -> getString(R.string.peppy_google_messages_link_notif_text_attention)
            status.state == "pairing" -> getString(R.string.peppy_google_messages_link_notif_text_pairing)
            status.running && status.phoneResponding -> getString(R.string.peppy_google_messages_link_notif_text_connected)
            status.running -> getString(R.string.peppy_google_messages_link_notif_text_phone_not_responding)
            else -> getString(R.string.peppy_google_messages_link_notif_text_reconnecting)
        }
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.peppy_logo)
            .setContentTitle(getString(R.string.peppy_google_messages_link_notif_title))
            .setContentText(text)
            .setContentIntent(PendingIntent.getActivity(this, NOTIFICATION_ID, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setVisibility(NotificationCompat.VISIBILITY_SECRET)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setOngoing(!attention)
            .setAutoCancel(attention)
            .build()
    }

    private fun foregroundServiceType(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING else 0

    companion object {
        internal const val CHANNEL_ID = "google_messages_link"
        internal const val NOTIFICATION_ID = 4110
        private const val PAIRING_WAIT_MS = 200L
        private val ATTENTION_STATES = setOf("needs_relink", "signed_out")
        @Volatile private var active: GoogleMessagesLinkService? = null

        fun createNotificationChannel(context: Context) {
            val manager = context.getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(NotificationChannel(CHANNEL_ID, context.getString(R.string.peppy_google_messages_link_notif_channel_name), NotificationManager.IMPORTANCE_LOW).apply {
                description = context.getString(R.string.peppy_google_messages_link_notif_channel_desc)
                setShowBadge(false)
                lockscreenVisibility = Notification.VISIBILITY_SECRET
            })
        }

        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, GoogleMessagesLinkService::class.java))
            } catch (_: IllegalStateException) {
            } catch (_: SecurityException) {
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, GoogleMessagesLinkService::class.java))
            cancelAttention(context)
        }

        fun cancelAttention(context: Context) {
            NotificationManagerCompat.from(context).cancel(NOTIFICATION_ID)
        }

        internal fun resetBackoff() {
            active?.backoff?.reset()
        }
    }

    override fun onCreate() {
        super.onCreate()
        active = this
    }

    override fun onDestroy() {
        if (active === this) active = null
        super.onDestroy()
    }
}
