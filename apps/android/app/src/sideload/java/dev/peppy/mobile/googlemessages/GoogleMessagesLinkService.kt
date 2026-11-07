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
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.peppy.mobile.MainActivity
import dev.peppy.mobile.R
import java.util.concurrent.Executor
import java.util.concurrent.Executors

internal data class ServiceTestHooks(
    val elapsedRealtime: () -> Long = SystemClock::elapsedRealtime,
    val sleeper: (Long) -> Unit = Thread::sleep,
    val notificationExecutor: Executor = DEFAULT_NOTIFICATION_EXECUTOR,
    val closeExecutor: Executor = DEFAULT_CLOSE_EXECUTOR,
    val notificationPublished: (GoogleMessagesLinkService) -> Unit = {},
    val afterStatusRead: () -> Unit = {},
    val beforeRun: () -> Unit = {},
    val workerFinished: () -> Unit = {},
)

private val DEFAULT_NOTIFICATION_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_CLOSE_EXECUTOR = Executors.newSingleThreadExecutor()

class GoogleMessagesLinkService : Service() {
    private val lock = Any()
    @Volatile private var foregroundUpdatesFenced = false
    private val backoff = RunBackoff()
    private var worker: Thread? = null
    private var workerLease: LinkHandleLease? = null
    private var leaseReservation = 0L
    private var requestedGeneration = 0L
    private var lifecycleGeneration = 0L
    private var lastStartId = 0
    @Volatile private var cancelled = false
    private var lastNotification: String? = null

    override fun onCreate() {
        super.onCreate()
        lifecycleGeneration = replaceActive(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        publishStatus(LinkStatus(0, "pairing", false, false, false, 0, ""))
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
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        } finally {
            synchronized(lock) {
                if (!cancelled && active === this) {
                    if (generation != requestedGeneration) {
                        startWorkerLocked(requestedGeneration)
                    } else {
                        worker = null
                        stopSelf(lastStartId)
                    }
                }
            }
            testHooks.workerFinished()
        }
    }

    private fun waitForLinkedRun() {
        val waitStartedAt = testHooks.elapsedRealtime()
        val lease = acquireWorkerLease() ?: return
        val handle = lease.handle
        while (isCurrent(lifecycleGeneration)) {
            val token = LinkCoreBridge.observationToken()
            val status = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            testHooks.afterStatusRead()
            if (!LinkCoreBridge.recordObservation(token, lease, applicationContext, status)) return
            publishStatus(status)
            when (nextServiceAction(status, LinkRuntime.pairingInProgress.get(), status.running)) {
                ServiceAction.STOP -> {
                    publishAttentionIfNeeded(status)
                    return
                }
                ServiceAction.WAIT -> {
                    if (testHooks.elapsedRealtime() - waitStartedAt >= MAX_PAIRING_WAIT_MS) return
                    if (!sleepInterruptibly(PAIRING_WAIT_MS)) return
                }
                ServiceAction.RUN -> return runWithRetry(lease)
            }
        }
    }

    private fun acquireWorkerLease(): LinkHandleLease? {
        val reservation = synchronized(lock) {
            if (cancelled || active !== this) return null
            ++leaseReservation
        }
        if (!LinkCoreBridge.recordPreStart(applicationContext)) return null
        val lease = LinkRuntime.handleLease(this) ?: return null
        return synchronized(lock) {
            if (cancelled || active !== this || leaseReservation != reservation) null
            else lease.also { workerLease = it }
        }
    }

    private fun runWithRetry(lease: LinkHandleLease) {
        val handle = lease.handle
        while (isCurrent(lifecycleGeneration)) {
            val beforeRunToken = LinkCoreBridge.observationToken()
            val beforeRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (!LinkCoreBridge.recordObservation(beforeRunToken, lease, applicationContext, beforeRun)) return
            if (beforeRun.state != "linked") return
            publishStatus(beforeRun)
            testHooks.beforeRun()
            val failure = runCatching { handle.run() }.exceptionOrNull()
            if (filterReasonCode(failure?.message) == "event_overflow") {
                LinkRuntimeEventSink.record("link", "event_overflow", 0)
            }
            if (!isCurrent(lifecycleGeneration) || filterReasonCode(failure?.message) in STOP_CODES) return
            val afterRunToken = LinkCoreBridge.observationToken()
            val afterRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (!LinkCoreBridge.recordObservation(afterRunToken, lease, applicationContext, afterRun)) return
            publishStatus(afterRun)
            if (afterRun.state != "linked") {
                publishAttentionIfNeeded(afterRun)
                return
            }
            if (failure == null || !sleepInterruptibly(backoff.nextDelayMillis())) return
        }
    }

    private fun sleepInterruptibly(delayMs: Long): Boolean = try {
        testHooks.sleeper(delayMs)
        !cancelled
    } catch (_: InterruptedException) {
        false
    }

    private fun cancelWorker() {
        cancelled = true
        val captured = synchronized(lock) {
            leaseReservation += 1
            worker?.interrupt()
            workerLease?.also { workerLease = null }
        }
        captured?.let { lease ->
            LinkRuntime.detachHandleForServiceStop(lease)?.let { handle ->
                testHooks.closeExecutor.execute { LinkRuntime.closeDetachedHandle(handle) }
            }
        }
    }

    private fun joinWorker() {
        val current = synchronized(lock) { worker }
        if (current != null && current !== Thread.currentThread()) runCatching { current.join(SERVICE_STOP_JOIN_MS) }
    }

    private fun refreshNotificationAfterEvent() {
        val generation = lifecycleGeneration
        testHooks.notificationExecutor.execute {
            if (!isCurrent(generation)) return@execute
            val lease = synchronized(lock) { workerLease }
                ?: LinkRuntime.handleLease(this)
                ?: return@execute
            val handle = lease.handle
            val token = LinkCoreBridge.observationToken()
            val status = runCatching { LinkStatus.parse(handle.status()) }.getOrNull() ?: return@execute
            if (!LinkCoreBridge.recordObservation(token, lease, applicationContext, status)) return@execute
            if (!isCurrent(generation)) return@execute
            publishStatusNow(status, generation)
        }
    }

    private fun publishStatus(status: LinkStatus) {
        val generation = lifecycleGeneration
        testHooks.notificationExecutor.execute { publishStatusNow(status, generation) }
    }

    private fun publishStatusNow(status: LinkStatus, generation: Long) {
        synchronized(activeLock) {
            if (!isCurrent(generation) || foregroundUpdatesFenced) return
            val key = "${status.state}:${status.running}:${status.phoneResponding}"
            if (lastNotification == key) return
            lastNotification = key
            if (!isCurrent(generation) || foregroundUpdatesFenced) return
            testHooks.notificationPublished(this)
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(status), foregroundServiceType())
        }
    }

    private fun publishAttentionIfNeeded(status: LinkStatus) {
        if (status.state !in ATTENTION_STATES) return
        val generation = lifecycleGeneration
        foregroundUpdatesFenced = true
        synchronized(activeLock) {
            if (!isCurrent(generation)) return
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_DETACH)
            if (!isCurrent(generation)) return
            testHooks.notificationPublished(this)
            postAttentionNotification(status)
        }
    }

    private fun isCurrent(generation: Long): Boolean =
        !cancelled && active === this && lifecycleGeneration == generation && serviceGeneration == generation

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
        private const val MAX_PAIRING_WAIT_MS = 30_000L + 30_000L + 300_000L + 30_000L
        private val STOP_CODES = setOf("link_closed", "unlink_in_progress", "pair_cancelled", "run_refused")
        private val ATTENTION_STATES = setOf("needs_relink", "signed_out")
        private val PRE_START_STATUS = LinkStatus(0, "unknown", false, false, false, 0, "")
        @Volatile private var active: GoogleMessagesLinkService? = null
        @Volatile private var serviceGeneration = 0L
        private val activeLock = Any()
        internal var testHooks = ServiceTestHooks()

        fun createNotificationChannel(context: Context) {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL_ID, context.getString(R.string.peppy_google_messages_link_notif_channel_name), NotificationManager.IMPORTANCE_LOW).apply {
                description = context.getString(R.string.peppy_google_messages_link_notif_channel_desc)
                setShowBadge(false)
                lockscreenVisibility = Notification.VISIBILITY_SECRET
            })
        }
        fun start(context: Context) { try { ContextCompat.startForegroundService(context, Intent(context, GoogleMessagesLinkService::class.java)) } catch (_: IllegalStateException) {} catch (_: SecurityException) {} }
        fun stop(context: Context) { stopAndAwait(context); cancelAttention(context) }
        fun stopAndAwait(context: Context) { active?.let { it.cancelWorker(); it.joinWorker() }; context.stopService(Intent(context, GoogleMessagesLinkService::class.java)) }
        internal fun stopActiveWorker() { active?.cancelWorker() }
        fun cancelAttention(context: Context) { NotificationManagerCompat.from(context).cancel(NOTIFICATION_ID) }
        internal fun onLinkEvent(kind: String, code: String) {
            active?.let { service ->
                if (kind == "conn" && code == "ready") service.backoff.reset()
                service.refreshNotificationAfterEvent()
            }
        }
        private fun replaceActive(service: GoogleMessagesLinkService): Long = synchronized(activeLock) {
            active?.cancelWorker()
            serviceGeneration += 1
            active = service
            serviceGeneration
        }
        private fun clearActive(service: GoogleMessagesLinkService) = synchronized(activeLock) {
            if (active === service) active = null
        }
        private fun canPostNotifications(context: Context): Boolean =
            (Build.VERSION.SDK_INT < 33 || ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) &&
                NotificationManagerCompat.from(context).areNotificationsEnabled()
        private const val SERVICE_STOP_JOIN_MS = 2_000L
    }
}
