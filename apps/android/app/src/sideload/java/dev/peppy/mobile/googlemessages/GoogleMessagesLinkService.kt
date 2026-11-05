package dev.peppy.mobile.googlemessages

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.peppy.mobile.MainActivity
import dev.peppy.mobile.R

class GoogleMessagesLinkService : Service() {
    private val lock = Any()
    private val backoff = RunBackoff()
    private var worker: Thread? = null
    private var requestedGeneration = 0L
    private var lastStartId = 0
    @Volatile private var cancelled = false
    private var lastNotification: String? = null

    override fun onCreate() {
        super.onCreate()
        replaceActive(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForegroundFor(LinkStatus(0, "pairing", false, false, true, 0, ""))
        synchronized(lock) {
            lastStartId = startId
            requestedGeneration += 1
            if (worker == null || worker?.isAlive != true) startWorkerLocked(requestedGeneration)
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        cancelWorker()
        clearActive(this)
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startWorkerLocked(generation: Long) {
        cancelled = false
        worker = Thread { runWorker(generation) }.also { it.start() }
    }

    private fun runWorker(generation: Long) {
        try {
            waitForLinkedRun()
        } finally {
            synchronized(lock) {
                if (cancelled || active !== this) return@synchronized
                if (generation != requestedGeneration) {
                    startWorkerLocked(requestedGeneration)
                    return@synchronized
                }
                worker = null
                stopSelf(lastStartId)
            }
        }
    }

    private fun waitForLinkedRun() {
        val waitStartedAt = nowMillis()
        while (!cancelled && active === this) {
            val handle = (LinkRuntime.handle(this) as? LinkOpenResult.Open)?.handle ?: return
            val status = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            startForegroundFor(status)
            when (nextServiceAction(status, LinkRuntime.pairingInProgress.get(), status.running)) {
                ServiceAction.STOP -> {
                    showAttentionIfNeeded(status)
                    return
                }
                ServiceAction.WAIT -> {
                    if (nowMillis() - waitStartedAt >= MAX_PAIRING_WAIT_MS) return
                    if (!sleepInterruptibly(PAIRING_WAIT_MS)) return
                }
                ServiceAction.RUN -> runWithRetry(handle)
            }
        }
    }

    private fun runWithRetry(handle: LinkHandle) {
        while (!cancelled && active === this) {
            val beforeRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (beforeRun.state != "linked") return
            startForegroundFor(beforeRun)
            val failure = runCatching { handle.run() }.exceptionOrNull()
            if (cancelled || active !== this || filterReasonCode(failure?.message) in STOP_CODES) return
            val afterRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            startForegroundFor(afterRun)
            if (afterRun.state != "linked") {
                showAttentionIfNeeded(afterRun)
                return
            }
            if (failure == null || !sleepInterruptibly(backoff.nextDelayMillis())) return
        }
    }

    private fun sleepInterruptibly(delayMs: Long): Boolean = try {
        sleeper(delayMs)
        !cancelled
    } catch (_: InterruptedException) {
        false
    }

    private fun cancelWorker() {
        cancelled = true
        LinkRuntime.stopRunningHandle()
        synchronized(lock) { worker?.interrupt() }
    }

    private fun refreshNotificationAfterEvent() {
        Thread {
            if (cancelled || active !== this) return@Thread
            val handle = (LinkRuntime.handle(this) as? LinkOpenResult.Open)?.handle ?: return@Thread
            runCatching { LinkStatus.parse(handle.status()) }.getOrNull()?.let(::startForegroundFor)
        }.start()
    }

    private fun startForegroundFor(status: LinkStatus) {
        val key = "${status.state}:${status.running}:${status.phoneResponding}"
        if (lastNotification == key) return
        lastNotification = key
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(status), foregroundServiceType())
    }

    private fun showAttentionIfNeeded(status: LinkStatus) {
        if (status.state !in ATTENTION_STATES) return
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_DETACH)
        postAttentionNotification(status)
    }

    private fun notification(status: LinkStatus, attention: Boolean = false): Notification {
        val text = when {
            attention -> getString(R.string.peppy_google_messages_link_notif_text_attention)
            status.state == "pairing" -> getString(R.string.peppy_google_messages_link_notif_text_pairing)
            status.running && status.phoneResponding -> getString(R.string.peppy_google_messages_link_notif_text_connected)
            status.running -> getString(R.string.peppy_google_messages_link_notif_text_phone_not_responding)
            else -> getString(R.string.peppy_google_messages_link_notif_text_reconnecting)
        }
        return NotificationCompat.Builder(this, CHANNEL_ID).setSmallIcon(R.drawable.peppy_logo)
            .setContentTitle(getString(R.string.peppy_google_messages_link_notif_title)).setContentText(text)
            .setContentIntent(PendingIntent.getActivity(this, NOTIFICATION_ID, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
            .setCategory(NotificationCompat.CATEGORY_SERVICE).setVisibility(NotificationCompat.VISIBILITY_SECRET)
            .setOnlyAlertOnce(true).setShowWhen(false).setOngoing(!attention).setAutoCancel(attention).build()
    }

    private fun foregroundServiceType(): Int = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING else 0

    private fun postAttentionNotification(status: LinkStatus) {
        if (!canPostNotifications(this)) return
        try {
            NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification(status, attention = true))
        } catch (_: SecurityException) {
            // The owner can revoke notification permission after the check.
        }
    }

    companion object {
        internal const val CHANNEL_ID = "google_messages_link"
        internal const val NOTIFICATION_ID = 4110
        private const val PAIRING_WAIT_MS = 200L
        private const val MAX_PAIRING_WAIT_MS = 5 * 60_000L + 30_000L
        private val STOP_CODES = setOf("link_closed", "unlink_in_progress", "pair_cancelled", "run_refused")
        private val ATTENTION_STATES = setOf("needs_relink", "signed_out")
        @Volatile private var active: GoogleMessagesLinkService? = null
        internal var sleeper: (Long) -> Unit = Thread::sleep
        internal var nowMillis: () -> Long = System::currentTimeMillis

        fun createNotificationChannel(context: Context) {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL_ID, context.getString(R.string.peppy_google_messages_link_notif_channel_name), NotificationManager.IMPORTANCE_LOW).apply {
                description = context.getString(R.string.peppy_google_messages_link_notif_channel_desc)
                setShowBadge(false)
                lockscreenVisibility = Notification.VISIBILITY_SECRET
            })
        }
        fun start(context: Context) { try { ContextCompat.startForegroundService(context, Intent(context, GoogleMessagesLinkService::class.java)) } catch (_: IllegalStateException) {} catch (_: SecurityException) {} }
        fun stop(context: Context) { stopAndAwait(context); cancelAttention(context) }
        fun stopAndAwait(context: Context) { active?.cancelWorker(); context.stopService(Intent(context, GoogleMessagesLinkService::class.java)) }
        internal fun stopActiveWorker() { active?.cancelWorker() }
        fun cancelAttention(context: Context) { NotificationManagerCompat.from(context).cancel(NOTIFICATION_ID) }
        internal fun onLinkEvent(kind: String, code: String) {
            active?.let { service ->
                if (kind == "conn" && code == "ready") service.backoff.reset()
                service.refreshNotificationAfterEvent()
            }
        }
        private fun replaceActive(service: GoogleMessagesLinkService) { active?.cancelWorker(); active = service }
        private fun clearActive(service: GoogleMessagesLinkService) { if (active === service) active = null }
        private fun canPostNotifications(context: Context): Boolean =
            (Build.VERSION.SDK_INT < 33 || ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) &&
                NotificationManagerCompat.from(context).areNotificationsEnabled()
    }
}
