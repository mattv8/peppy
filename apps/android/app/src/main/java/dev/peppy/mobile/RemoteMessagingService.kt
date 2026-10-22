package dev.peppy.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.security.MessageDigest
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

internal object RemoteMessagingProtocol {
    /** The server permits roughly 1 MiB replay frames. Hints never acknowledge those frames. */
    const val MAX_FRAME_BYTES = 1_100_000
    private const val SMALL_FRAME_BYTES = 64 * 1024
    private const val LARGE_FRAME_PREFIX_CHARS = 4 * 1024
    private val LARGE_EVENT = Regex(""""type"\s*:\s*"event"""")

    fun hello(cursor: String): String = JSONObject().put("protocol_version", 1).put("resume_cursor", cursor).toString()
    fun frameType(frame: String): String? {
        val bytes = frame.toByteArray(Charsets.UTF_8).size
        if (bytes > MAX_FRAME_BYTES) return null
        if (bytes <= SMALL_FRAME_BYTES) return try {
            JSONObject(frame).optString("type").takeIf { it.isNotEmpty() }
        } catch (_: Exception) {
            null
        }
        // Replay bodies may be near 1 MiB. They never need parsing here; only a server event hint
        // is useful, and the tagged envelope is emitted before its encrypted body.
        return if (LARGE_EVENT.containsMatchIn(frame.take(LARGE_FRAME_PREFIX_CHARS))) "event" else null
    }
    fun shouldSync(frame: String): Boolean = frameType(frame) in setOf("ready", "event", "resync_required")
}

internal object RemoteMessagingBackoff {
    fun delayMs(attempt: Int, jitter: Double): Long {
        val base = minOf(30_000L, 1_000L shl attempt.coerceIn(0, 5))
        return (base * (1.0 + jitter.coerceIn(0.0, 0.5))).toLong()
    }
}

internal enum class RemoteMessagingStatus { CONNECTED, RECONNECTING, LOCKED }

/** Generation fence shared by service callbacks and lifecycle teardown. */
internal class RemoteMessagingFence {
    private val stopped = AtomicBoolean(false)
    private val destroyed = AtomicBoolean(false)
    private val generation = AtomicLong(0)
    fun next(): Long = generation.incrementAndGet()
    fun stop() { stopped.set(true); generation.incrementAndGet() }
    fun start(): Long? {
        if (destroyed.get()) return null
        if (stopped.compareAndSet(true, false)) generation.incrementAndGet()
        return generation.get()
    }
    fun destroy() { destroyed.set(true); stop() }
    fun current() = generation.get()
    fun accepts(token: Long) = !stopped.get() && token == generation.get()
}

/** Runtime-only state: a process restart must never claim that an old socket remains connected. */
internal object RemoteMessagingRuntime {
    @Volatile private var current = RemoteMessagingStatus.LOCKED
    fun status() = current
    fun set(status: RemoteMessagingStatus) { current = status }
}

internal object RemoteMessagingPreferences {
    private const val FILE = "peppy-remote-messaging"
    private const val ENABLED = "enabled.v1"
    fun enabled(context: Context) = context.getSharedPreferences(FILE, 0).getBoolean(ENABLED, true)
    fun setEnabled(context: Context, enabled: Boolean) = context.getSharedPreferences(FILE, 0).edit().putBoolean(ENABLED, enabled).apply()
}

/** A serialized hint transport. Durable replay and every carrier effect stay in [GatewaySyncWorker]. */
class RemoteMessagingService : Service() {
    private val dispatcher = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)
    private val client = OkHttpClient.Builder().pingInterval(25, TimeUnit.SECONDS).followRedirects(false).build()
    private val fence = RemoteMessagingFence()
    private var socket: WebSocket? = null // accessed only on [dispatcher]
    private var activeIdentity: String? = null
    private var reconnects = 0
    private var connected = false
    private var reconnectJob: Job? = null
    private lateinit var connectivity: ConnectivityManager
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = serialized(fence.current()) { scheduleReconnect(fence.current()) }
        override fun onLost(network: Network) = serialized(fence.current()) { scheduleReconnect(fence.current()) }
    }

    override fun onCreate() {
        super.onCreate()
        connectivity = getSystemService(ConnectivityManager::class.java)
        connectivity.registerDefaultNetworkCallback(networkCallback)
        ensureChannel()
        updateStatus(RemoteMessagingStatus.RECONNECTING)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        scope.launch { fence.start()?.let { startIfEligible(it) } }
        return START_NOT_STICKY
    }

    private fun startIfEligible(startToken: Long) {
        if (!isCurrent(startToken)) return
        if (!RemoteMessagingPreferences.enabled(this)) return stopLocked()
        val session = NativeGateway.session(this) ?: return stopLocked()
        val identity = identity(session)
        if (identity == activeIdentity && socket != null) return
        val token = fence.next()
        cancelSocket()
        activeIdentity = identity
        reconnects = 0
        connected = false
        connect(session, token)
    }

    private fun connect(session: GatewaySession, token: Long) {
        if (!isCurrent(token)) return
        reconnectJob?.cancel()
        reconnectJob = null
        cancelSocket()
        updateStatus(RemoteMessagingStatus.RECONNECTING)
        socket = client.newWebSocket(GatewayHttp(session.origin, session.bearerToken).webSocketRequest(), listener(session, token))
    }

    private fun listener(session: GatewaySession, token: Long) = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) = serialized(token, webSocket) {
            if (!webSocket.send(RemoteMessagingProtocol.hello(session.client.receiveCursor()))) scheduleReconnect(token)
        }

        override fun onMessage(webSocket: WebSocket, text: String) = serialized(token, webSocket) {
            when (RemoteMessagingProtocol.frameType(text)) {
                "ready" -> if (!connected) {
                    connected = true
                    reconnects = 0
                    updateStatus(RemoteMessagingStatus.CONNECTED)
                    GatewayWork.enqueueUrgent(applicationContext)
                }
                "event", "resync_required" -> GatewayWork.enqueueUrgent(applicationContext)
                // A valid large replay is intentionally ignored as a hint: HTTP replay owns durable ingestion.
            }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) = serialized(token, webSocket) {
            if (response?.code == 401 || response?.code == 403) stopLocked() else scheduleReconnect(token)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = serialized(token, webSocket) {
            if (code == 4401) stopLocked() else scheduleReconnect(token)
        }
    }

    private fun serialized(token: Long, callbackSocket: WebSocket? = null, action: () -> Unit) {
        scope.launch { if (isCurrent(token) && (callbackSocket == null || socket === callbackSocket)) action() }
    }

    private fun scheduleReconnect(token: Long) {
        if (!isCurrent(token)) return
        if (!RemoteMessagingPreferences.enabled(this) || NativeGateway.session(this) == null) return stopLocked()
        connected = false
        val reconnectToken = fence.next()
        cancelSocket()
        reconnectJob?.cancel()
        updateStatus(RemoteMessagingStatus.RECONNECTING)
        val attempt = reconnects++
        reconnectJob = scope.launch {
            kotlinx.coroutines.delay(RemoteMessagingBackoff.delayMs(attempt, Math.random() * 0.5))
            if (!isCurrent(reconnectToken)) return@launch
            val session = NativeGateway.session(applicationContext) ?: return@launch stopLocked()
            val currentIdentity = identity(session)
            if (currentIdentity != activeIdentity) {
                activeIdentity = currentIdentity
                reconnects = 0
                connect(session, fence.next())
            } else connect(session, reconnectToken)
        }
    }

    private fun isCurrent(token: Long) = fence.accepts(token)
    private fun cancelSocket() { socket?.cancel(); socket = null }
    private fun stopLocked() {
        fence.stop()
        reconnectJob?.cancel()
        reconnectJob = null
        cancelSocket()
        connected = false
        updateStatus(RemoteMessagingStatus.LOCKED)
        stopSelf()
    }

    private fun updateStatus(status: RemoteMessagingStatus) {
        RemoteMessagingRuntime.set(status)
        startForeground(NOTIFICATION_ID, notification(status))
    }

    private fun ensureChannel() {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.peppy_remote_messaging_notif_channel_name), NotificationManager.IMPORTANCE_LOW).apply {
                description = getString(R.string.peppy_remote_messaging_notif_channel_desc)
            },
        )
    }

    private fun notification(status: RemoteMessagingStatus): Notification {
        val text = when (status) {
            RemoteMessagingStatus.CONNECTED -> getString(R.string.peppy_remote_messaging_notif_active)
            RemoteMessagingStatus.RECONNECTING -> getString(R.string.peppy_remote_messaging_notif_reconnecting)
            RemoteMessagingStatus.LOCKED -> getString(R.string.peppy_remote_messaging_notif_locked)
        }
        val intent = Intent(this, MainActivity::class.java).apply { if (status == RemoteMessagingStatus.LOCKED) action = ACTION_UNLOCK }
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.peppy_logo)
            .setContentTitle(getString(R.string.peppy_app_name))
            .setContentText(text)
            .setContentIntent(PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
            .setOngoing(true)
            .build()
    }

    override fun onDestroy() {
        fence.destroy()
        connectivity.unregisterNetworkCallback(networkCallback)
        scope.launch {
            reconnectJob?.cancel()
            reconnectJob = null
            cancelSocket()
            connected = false
            RemoteMessagingRuntime.set(RemoteMessagingStatus.LOCKED)
            scope.cancel()
            dispatcher.close()
            client.dispatcher.executorService.shutdown()
        }
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun identity(session: GatewaySession): String = "${session.origin}\n${session.deviceId}\n" +
        MessageDigest.getInstance("SHA-256").digest(session.bearerToken.toByteArray()).joinToString("") { "%02x".format(it) }

    companion object {
        const val ACTION_UNLOCK = "peppy.action.UNLOCK"
        private const val CHANNEL = "remote_messaging"
        private const val NOTIFICATION_ID = 91

        /**
         * Safe wake seam for callers on the main thread. It never opens native session state here:
         * the service checks the session on its serialized dispatcher and stops itself if locked or ineligible.
         */
        fun resumeOrEnqueue(context: Context) {
            // A wake may be delivered while TCP looks open but can no longer carry data.
            GatewayWork.enqueueUrgent(context)
            if (!RemoteMessagingPreferences.enabled(context)) return
            try {
                ContextCompat.startForegroundService(context, Intent(context, RemoteMessagingService::class.java))
            } catch (_: IllegalStateException) {
            } catch (_: SecurityException) {
            }
        }

        fun stop(context: Context) {
            RemoteMessagingRuntime.set(RemoteMessagingStatus.LOCKED)
            context.stopService(Intent(context, RemoteMessagingService::class.java))
        }
    }
}

/** Remote-messaging is a boot-allowlisted Android 15+ FGS type; failure still falls back to sync. */
class RemoteMessagingBootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        RemoteMessagingService.resumeOrEnqueue(context)
    }
}
