package dev.peppy.mobile.googlemessages.live

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.SystemClock
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import java.security.SecureRandom
import java.util.Base64
import java.util.concurrent.Executors
import java.util.concurrent.Executor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString

data class LiveTypingCredentials(
    val origin: String,
    val bearerToken: String,
    val localDeviceId: String,
)

data class LiveTypingEnableContext(
    val rosterIds: List<String>,
    val localDeviceId: String,
    val publication: Long = 0L,
)

interface LiveTypingListener {
    fun onEnabled(context: LiveTypingEnableContext)
    fun onFrame(json: String)
    fun onDisabled(reason: LiveTypingDisabledReason)
}

enum class LiveTypingDisabledReason {
    STOPPED,
    AUTHENTICATION_FAILED,
    MEMBERSHIP_REVOKED,
    PROTOCOL_ERROR,
    CONNECTION_LOST,
}

/**
 * Serialized bearer-mode `/v1/live` host. It owns only transport lifetime; callers own core calls
 * and must invoke them from listener callbacks without blocking the main thread.
 */
class LiveTypingHost private constructor(
    private val socketFactory: (Request, WebSocketListener) -> WebSocket,
    private val connectivity: ConnectivityManager?,
    private val scheduler: LiveScheduler,
    private val clock: LiveClock,
    private val executor: Executor = Executors.newSingleThreadScheduledExecutor(),
) {
    private val random = SecureRandom()
    private var socket: WebSocket? = null
    private var connection: LiveConnection? = null
    private var credentials: LiveTypingCredentials? = null
    private var listener: LiveTypingListener? = null
    private var retry: LiveScheduledTask? = null
    private var membershipTask: LiveScheduledTask? = null
    private var bootstrapTask: LiveScheduledTask? = null
    private var lastChallengeAt = Long.MIN_VALUE
    private var attempt = 0
    private var protocolRejections = 0
    @Volatile private var generation = 0L
    @Volatile private var ready = false
    private val readinessLock = Any()
    private var activePublication: Long? = null
    private var membershipPublication = 0L
    private var disabledNotified = false
    @Volatile private var stopped = true
    private var networkRegistered = false

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = execute(::networkChanged)
        override fun onLost(network: Network) = execute(::networkChanged)
    }

    constructor(context: Context) : this(defaultSocketFactory(), context.getSystemService(ConnectivityManager::class.java), ExecutorScheduler(executorForScheduler()), ElapsedClock)

    fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) = execute {
        stopLocked(LiveTypingDisabledReason.STOPPED, notify = false)
        this.credentials = credentials
        this.listener = listener
        stopped = false
        protocolRejections = 0
        resetDisableNotification()
        registerNetworkCallback()
        connect()
    }

    fun stop() = execute { stopLocked(LiveTypingDisabledReason.STOPPED, notify = true) }

    internal fun reconnectForTest() = execute(::networkChanged)

    /** Returns false unless this membership publication still authorizes the current socket. */
    fun send(frameJson: String, publication: Long): Boolean {
        val capturedSocket = socket
        if (capturedSocket == null || !isOutboundFrame(frameJson) || !isPublicationCurrent(publication)) return false
        executor.execute {
            if (!stopped && socket === capturedSocket && isPublicationCurrent(publication)) capturedSocket.send(frameJson)
        }
        return true
    }

    private fun connect() {
        if (stopped || socket != null) return
        val currentCredentials = credentials ?: return stopLocked(LiveTypingDisabledReason.STOPPED, true)
        val token = ++generation
        val core = object : LiveCore {
            override fun enable(snapshot: LiveMembershipSnapshot) {
                if (isCurrent(token)) markEnabled(token, snapshot, currentCredentials.localDeviceId)
            }

            override fun disable() {
                if (isCurrent(token)) markDisabled(LiveTypingDisabledReason.MEMBERSHIP_REVOKED)
            }
        }
        connection = LiveConnection(clock, core, currentCredentials.localDeviceId)
        socket = socketFactory(requestFor(currentCredentials), socketListener(token))
    }

    private fun socketListener(token: Long) = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) = serialized(token, webSocket) {
            bootstrapTask = scheduler.schedule(BOOTSTRAP_TIMEOUT_MS) {
                serialized(token, webSocket) { reconnect(LiveTypingDisabledReason.CONNECTION_LOST) }
            }
        }

        override fun onMessage(webSocket: WebSocket, text: String) = serialized(token, webSocket) {
            if (utf8Bytes(text) > LiveTransport.MAX_CONTROL_BYTES) return@serialized protocolFailure()
            val control = MembershipControls.parse(text)
            when (control) {
                is MembershipControl.Bootstrap -> {
                    if (connection?.bootstrap(control) != true) protocolFailure() else {
                        bootstrapTask?.cancel()
                        bootstrapTask = null
                        bootstrapConnectionId = control.connectionId
                        driveChallenge()
                    }
                }
                is MembershipControl.Response -> {
                    if (connection?.response(control) != true) {
                        if (connection?.selfRevoked() == true) stopLocked(LiveTypingDisabledReason.MEMBERSHIP_REVOKED, true)
                        else reconnect(LiveTypingDisabledReason.CONNECTION_LOST)
                    } else {
                        attempt = 0
                        scheduleNextChallenge()
                    }
                }
                is MembershipControl.Hint -> {
                    if (connection?.hint(control) != true) protocolFailure() else scheduleNextChallenge()
                }
                null -> if (utf8Bytes(text) <= LiveTransport.MAX_TEXT_BYTES && ready) listener?.onFrame(text)
            }
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) = serialized(token, webSocket) { protocolFailure() }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) = serialized(token, webSocket) {
            if (response?.code == 401 || response?.code == 403) authFailure()
            else reconnect(LiveTypingDisabledReason.CONNECTION_LOST)
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) = serialized(token, webSocket) {
            webSocket.close(code, null)
            handleClose(code, reason)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = serialized(token, webSocket) {
            handleClose(code, reason)
        }
    }

    private fun driveChallenge() {
        membershipTask = null
        val current = connection ?: return protocolFailure()
        val currentSocket = socket ?: return protocolFailure()
        var enqueueFailed = false
        val nonce = current.drive(::nextNonce) { challengeId ->
            (socket === currentSocket && currentSocket.send(challengeJson(challengeId))).also { sent -> enqueueFailed = !sent }
        }
        if (enqueueFailed) return protocolFailure()
        if (nonce != null) {
            lastChallengeAt = clock.elapsedRealtime()
            val token = generation
            membershipTask = scheduler.schedule(CHALLENGE_RESPONSE_TIMEOUT_MS + 1) {
                serialized(token, currentSocket) {
                    membershipTask = null
                    if (connection?.tick() != true) reconnect(LiveTypingDisabledReason.CONNECTION_LOST)
                }
            }
        }
    }

    private fun scheduleNextChallenge() {
        membershipTask?.cancel()
        val elapsed = clock.elapsedRealtime() - lastChallengeAt
        val delay = (CHALLENGE_CADENCE_MS - elapsed).coerceAtLeast(0)
        val token = generation
        val currentSocket = socket ?: return protocolFailure()
        membershipTask = scheduler.schedule(delay) { serialized(token, currentSocket, ::driveChallenge) }
    }

    private fun challengeJson(challengeId: String): String {
        val connectionId = connectionIdFromBootstrap() ?: return ""
        return "{\"type\":\"membership_challenge\",\"version\":1,\"connection_id\":\"$connectionId\",\"challenge_id\":\"$challengeId\"}"
    }

    // Bootstrap IDs are opaque; retain the server-provided value for its matching challenge.
    private fun connectionIdFromBootstrap(): String? = bootstrapConnectionId

    private var bootstrapConnectionId: String? = null

    private fun nextNonce(): String = ByteArray(16).also(random::nextBytes).let { Base64.getUrlEncoder().withoutPadding().encodeToString(it) }

    private fun reconnect(reason: LiveTypingDisabledReason) {
        if (stopped) return
        retireSocket()
        markDisabled(reason)
        if (retry != null) return
        val delay = (1_000L shl attempt.coerceAtMost(5)).coerceAtMost(30_000L)
        attempt++
        recordGoogleMessagesLinkEvent("live", "reconnect_scheduled", 0)
        val token = generation
        retry = scheduler.schedule(delay) { execute {
            if (!stopped && token == generation) {
                retry = null
                connect()
            }
        } }
    }

    private fun networkChanged() {
        if (stopped) return
        retry?.cancel()
        retry = null
        attempt = 0
        retireSocket()
        markDisabled(LiveTypingDisabledReason.CONNECTION_LOST)
        connect()
    }

    private fun protocolFailure() {
        recordGoogleMessagesLinkEvent("live", "protocol_fatal", 0)
        resetDisableNotification()
        stopLocked(LiveTypingDisabledReason.PROTOCOL_ERROR, true)
    }

    private fun authFailure() {
        recordGoogleMessagesLinkEvent("live", "auth_refused", 0)
        stopLocked(LiveTypingDisabledReason.AUTHENTICATION_FAILED, true)
    }

    private fun handleClose(code: Int, reason: String) {
        when {
            code == 1008 && reason == "live_unauthorized" -> authFailure()
            code == 4401 && reason == "revoked" -> stopLocked(LiveTypingDisabledReason.MEMBERSHIP_REVOKED, true)
            code == 1008 && reason == "invalid_live_frame" -> {
                protocolRejections++
                if (protocolRejections >= MAX_PROTOCOL_REJECTIONS) protocolFailure()
                else reconnect(LiveTypingDisabledReason.CONNECTION_LOST)
            }
            else -> reconnect(LiveTypingDisabledReason.CONNECTION_LOST)
        }
    }

    private fun isCurrent(token: Long) = !stopped && token == generation

    private fun markEnabled(token: Long, snapshot: LiveMembershipSnapshot, localDeviceId: String) {
        val context = synchronized(readinessLock) {
            val publication = publication(token)
            ready = true
            activePublication = publication
            disabledNotified = false
            LiveTypingEnableContext(snapshot.activeRoster.map { it.deviceId }, localDeviceId, publication)
        }
        recordGoogleMessagesLinkEvent("live", "enabled", 0)
        listener?.onEnabled(context)
    }

    private fun markDisabled(reason: LiveTypingDisabledReason) {
        val notify = synchronized(readinessLock) {
            ready = false
            activePublication = null
            if (disabledNotified) false else {
                disabledNotified = true
                true
            }
        }
        if (notify) {
            recordGoogleMessagesLinkEvent("live", "disabled", 0)
            listener?.onDisabled(reason)
        }
    }

    private fun clearReadyWithoutNotification() = synchronized(readinessLock) {
        ready = false
        activePublication = null
        disabledNotified = true
    }

    private fun resetDisableNotification() = synchronized(readinessLock) {
        disabledNotified = false
    }

    private fun isPublicationCurrent(publication: Long) = synchronized(readinessLock) {
        ready && activePublication == publication
    }

    private fun publication(socketIncarnation: Long): Long {
        membershipPublication = (membershipPublication + 1) and PUBLICATION_MASK
        return (socketIncarnation shl PUBLICATION_BITS) or membershipPublication
    }

    private fun stopLocked(reason: LiveTypingDisabledReason, notify: Boolean) {
        retry?.cancel()
        retry = null
        val wasActive = !stopped
        stopped = true
        retireSocket()
        connection?.retire()
        connection = null
        bootstrapConnectionId = null
        credentials = null
        if (networkRegistered) {
            connectivity?.unregisterNetworkCallback(networkCallback)
            networkRegistered = false
        }
        if (notify && wasActive) markDisabled(reason) else clearReadyWithoutNotification()
        listener = null
    }

    private fun retireSocket() {
        membershipTask?.cancel()
        membershipTask = null
        bootstrapTask?.cancel()
        bootstrapTask = null
        lastChallengeAt = Long.MIN_VALUE
        bootstrapConnectionId = null
        socket?.close(1000, "closed")
        socket = null
        connection?.retire()
    }

    private fun registerNetworkCallback() {
        if (!networkRegistered && connectivity != null) {
            connectivity.registerDefaultNetworkCallback(networkCallback)
            networkRegistered = true
        }
    }

    private fun serialized(token: Long, callbackSocket: WebSocket, action: () -> Unit) = execute {
        if (!stopped && token == generation && socket === callbackSocket) action()
    }

    private fun execute(action: () -> Unit) = executor.execute(action)

    companion object {
        private const val CHALLENGE_CADENCE_MS = 20_000L
        private const val CHALLENGE_RESPONSE_TIMEOUT_MS = 5_000L
        // This starts at onOpen; the server starts its five-second budget later while processing the upgrade.
        private const val BOOTSTRAP_TIMEOUT_MS = 10_000L
        private const val MAX_PROTOCOL_REJECTIONS = 3
        private const val PUBLICATION_BITS = 32
        private const val PUBLICATION_MASK = 0xffff_ffffL
        private val ElapsedClock = object : LiveClock { override fun elapsedRealtime(): Long = SystemClock.elapsedRealtime() }
        private val TestClock = object : LiveClock { override fun elapsedRealtime(): Long = 0L }

        fun requestFor(credentials: LiveTypingCredentials): Request {
            val origin = java.net.URI(credentials.origin)
            require(origin.scheme == "https" || origin.scheme == "http")
            require(origin.host != null && origin.userInfo == null)
            val scheme = if (origin.scheme == "https") "wss" else "ws"
            val authority = origin.rawAuthority
            val url = "$scheme://$authority/v1/live"
            return Request.Builder().url(url).header("Authorization", "Bearer ${credentials.bearerToken}")
                .header("x-peppy-live-capabilities", "membership-version-v1").build()
        }

        internal fun forTest(
            factory: (Request, WebSocketListener) -> WebSocket,
            scheduler: LiveScheduler = ExecutorScheduler(executorForScheduler()),
            clock: LiveClock = TestClock,
            executor: Executor = Executors.newSingleThreadScheduledExecutor(),
        ) = LiveTypingHost(factory, null, scheduler, clock, executor)

        fun isOutboundFrame(frameJson: String) = utf8Bytes(frameJson) <= LiveTransport.MAX_TEXT_BYTES

        private fun defaultSocketFactory(): (Request, WebSocketListener) -> WebSocket {
            val client = OkHttpClient.Builder().pingInterval(25, TimeUnit.SECONDS).followRedirects(false).build()
            return { request, listener -> client.newWebSocket(request, listener) }
        }

        private fun executorForScheduler() = Executors.newSingleThreadScheduledExecutor()

        private fun utf8Bytes(value: String) = value.toByteArray(Charsets.UTF_8).size
    }
}

internal fun interface LiveScheduledTask { fun cancel() }

internal interface LiveScheduler { fun schedule(delayMs: Long, action: () -> Unit): LiveScheduledTask }

private class ExecutorScheduler(private val executor: java.util.concurrent.ScheduledExecutorService) : LiveScheduler {
    override fun schedule(delayMs: Long, action: () -> Unit): LiveScheduledTask {
        val future: ScheduledFuture<*> = executor.schedule(action, delayMs, TimeUnit.MILLISECONDS)
        return LiveScheduledTask { future.cancel(false) }
    }
}
