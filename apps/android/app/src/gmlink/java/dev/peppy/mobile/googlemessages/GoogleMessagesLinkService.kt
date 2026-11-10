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
import dev.peppy.mobile.RcsSendWake
import dev.peppy.mobile.R
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import uniffi.peppy_mobile_bindings.gatewayPendingNativeRcsCommands
import uniffi.peppy_mobile_bindings.rcsCanonicalizerRevision

internal data class ServiceTestHooks(
    val elapsedRealtime: () -> Long = SystemClock::elapsedRealtime,
    val sleeper: (Long) -> Unit = Thread::sleep,
    val notificationExecutor: Executor = DEFAULT_NOTIFICATION_EXECUTOR,
    val closeExecutor: Executor = DEFAULT_CLOSE_EXECUTOR,
    val notificationPublished: (GoogleMessagesLinkService) -> Unit = {},
    val afterStatusRead: () -> Unit = {},
    val beforeRun: () -> Unit = {},
    val workerFinished: () -> Unit = {},
    val receiveSweepExecutor: Executor = DEFAULT_RCS_RECEIVE_EXECUTOR,
    val receiveSweep: (LinkHandleLease, AtomicBoolean) -> Unit = LinkRuntime::resumeRcsReceive,
    val beforeEligibilityEnumeration: () -> Unit = {},
    val currentTimeMillis: () -> Long = System::currentTimeMillis,
    val bootstrapExecutor: Executor = DEFAULT_RCS_BOOTSTRAP_EXECUTOR,
    val eligibilityExecutor: Executor = DEFAULT_RCS_ELIGIBILITY_EXECUTOR,
    val bootstrapFactory: (Context, LinkHandleLease, () -> Boolean) -> RcsLinkBootstrap = { context, lease, current ->
        RcsLinkBootstrap(context, lease, current)
    },
    val typingBridgeFactory: (Context) -> TypingBridge = ::TypingBridge,
    val typingSession: (Context) -> TypingCredentials? = { context ->
        NativeGateway.session(context)?.let { TypingCredentials(it.origin, it.bearerToken, it.deviceId, it.generation) }
    },
    val typingExecutor: Executor = DEFAULT_TYPING_EXECUTOR,
)

private val DEFAULT_NOTIFICATION_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_CLOSE_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_RCS_RECEIVE_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_RCS_BOOTSTRAP_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_RCS_ELIGIBILITY_EXECUTOR = Executors.newSingleThreadExecutor()
private val DEFAULT_TYPING_EXECUTOR = Executors.newSingleThreadExecutor()

private data class ReceiveSweepRequest(
    val serviceGeneration: Long,
    val runGeneration: Long,
    val lease: LinkHandleLease,
    val cancelled: AtomicBoolean,
)

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
    private lateinit var nativeRcsSendWork: RcsNativeSendWork
    private lateinit var typingBridge: TypingBridge
    private var typingRetirementListener: (() -> Unit)? = null
    private var eligibilityPublisher: RcsEligibilityPublisher? = null
    private val eligibilityLock = Any()
    private var eligibilityDirty = false
    private var eligibilityScheduled = false
    private var bootstrapLease: LinkHandleLease? = null
    private var bootstrap: RcsLinkBootstrap? = null
    private lateinit var targetedBootstrap: RcsTargetedBootstrapDrain
    @Volatile private var lastBootstrapAtMs = 0L
    private val readinessLock = Any()
    private var readinessScheduled = false
    private var readinessPending = false
    private val receiveSweepLock = Any()
    private var receiveRunGeneration = 0L
    private var receiveRunLease: LinkHandleLease? = null
    private var pendingReceiveSweep: ReceiveSweepRequest? = null
    private var runningReceiveSweep: ReceiveSweepRequest? = null
    private var receiveSweepDrainScheduled = false

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel(this)
        // Created before this service becomes the active target, so targeted requests never see it uninitialized.
        targetedBootstrap = RcsTargetedBootstrapDrain(
            executor = Executor { },
            alive = { isCurrent(lifecycleGeneration) },
            run = ::runTargetedBootstrap,
        )
        nativeRcsSendWork = RcsNativeSendWork(
            enabled = ::nativeRcsEnabled,
            candidates = {
                NativeGateway.open(applicationContext)
                    ?.let(::gatewayPendingNativeRcsCommands)
                    .orEmpty()
                    .map { RcsNativeCommand(it.commandId, it.conversationId, it.recipient) }
            },
            execute = ::runNativeRcsSend,
            current = { isCurrent(lifecycleGeneration) },
            executor = NATIVE_RCS_SEND_EXECUTOR,
        )
        typingBridge = testHooks.typingBridgeFactory(applicationContext)
        TypingReceiveSinkRegistry.sink = typingBridge
        lifecycleGeneration = replaceActive(this)
        typingRetirementListener = NativeGateway.addRetirementListener { typingBridge.stop() }
        LinkCoreBridge.scheduleOwnerUnlinkCutover(applicationContext)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Foreground promotion must not wait behind a stale notification refresh.
        publishStatusNow(LinkStatus(0, "pairing", false, false, false, 0, ""), lifecycleGeneration)
        synchronized(lock) {
            lastStartId = startId
            requestedGeneration += 1
            if (worker == null || worker?.isAlive != true) startWorkerLocked(requestedGeneration)
        }
        nativeRcsSendWork.start()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        typingRetirementListener?.invoke()
        typingRetirementListener = null
        typingBridge.stop()
        if (TypingReceiveSinkRegistry.sink === typingBridge) TypingReceiveSinkRegistry.sink = null
        nativeRcsSendWork.stop()
        cancelWorker()
        clearActive(this)
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun runNativeRcsSend(command: RcsNativeCommand) {
        val lease = LinkRuntime.handleLease(this) ?: return
        val clock = RcsNativeClock(System::currentTimeMillis)
        RcsNativeSendCoordinator(
            RcsNativeGoAdapter(lease),
            RcsNativeCoreAdapter(applicationContext, clock),
            RcsNativeAndroidRouteReader(applicationContext, clock),
            clock,
            rcsCanonicalizerRevision().toInt(),
            RcsNativeAndroidRouteChangeRevoker(applicationContext),
        ).execute(command)
        publishNativeRcsEligibility(command.conversationId)
    }

    private fun publishNativeRcsEligibility(conversationId: String) {
        if (!nativeRcsEnabled()) return
        runCatching {
			val client = NativeGateway.open(applicationContext) ?: return
            val publisher = eligibilityPublisher ?: RcsEligibilityPublisher(
                GeneratedRcsEligibilityPublisherPort(client),
                current = { isCurrent(lifecycleGeneration) },
            ).also { eligibilityPublisher = it }
            publisher.publish(listOf(conversationId))
        }.onFailure { recordGoogleMessagesLinkEvent("link", "rcs_eligibility_failed", 0) }
    }

    private fun publishNativeRcsEligibility() {
        if (!nativeRcsEnabled()) return
        runCatching {
			testHooks.beforeEligibilityEnumeration()
            val client = NativeGateway.open(applicationContext) ?: return
            val publisher = eligibilityPublisher ?: RcsEligibilityPublisher(
                GeneratedRcsEligibilityPublisherPort(client),
                current = { isCurrent(lifecycleGeneration) },
            ).also { eligibilityPublisher = it }
            publisher.publish(client.gatewayRcsLinkOwnedConversations().map { it.conversationId })
        }.onFailure { recordGoogleMessagesLinkEvent("link", "rcs_eligibility_failed", 0) }
    }

    /** Conversation callbacks coalesce to one background eligibility publication without a timer or poll. */
    private fun queueNativeRcsEligibility() {
        if (!nativeRcsEnabled()) return
        val schedule = synchronized(eligibilityLock) {
            eligibilityDirty = true
            if (eligibilityScheduled) false else {
                eligibilityScheduled = true
                true
            }
        }
        if (!schedule) return
        try {
            testHooks.eligibilityExecutor.execute(::drainNativeRcsEligibility)
        } catch (_: RuntimeException) {
            synchronized(eligibilityLock) { eligibilityScheduled = false }
            recordGoogleMessagesLinkEvent("link", "rcs_eligibility_failed", 0)
        }
    }

    private fun drainNativeRcsEligibility() {
        while (true) {
            val publish = synchronized(eligibilityLock) {
                if (!eligibilityDirty) {
                    eligibilityScheduled = false
                    return
                }
                eligibilityDirty = false
                true
            }
            if (publish) publishNativeRcsEligibility()
        }
    }

    /** Coalesces readiness work; only full ownership bootstrap observes the five-minute throttle. */
    private fun requestRcsBootstrap() {
        targetedBootstrap.kick()
        val schedule = synchronized(readinessLock) {
            if (readinessScheduled) {
                readinessPending = true
                false
            } else {
                readinessScheduled = true
                true
            }
        }
        if (!schedule) return
        try {
            testHooks.bootstrapExecutor.execute(::drainRcsReadiness)
        } catch (_: RuntimeException) {
            synchronized(readinessLock) { readinessScheduled = false }
            recordGoogleMessagesLinkEvent("link", "rcs_bootstrap_failed", 0)
        }
    }

    private fun drainRcsReadiness() {
        while (true) {
            runCatching(::runRcsReadinessPass)
                .onFailure { recordGoogleMessagesLinkEvent("link", "rcs_bootstrap_failed", 0) }
            val rerun = synchronized(readinessLock) {
                if (readinessPending) {
                    readinessPending = false
                    true
                } else {
                    readinessScheduled = false
                    false
                }
            }
            if (!rerun) return
        }
    }

    private fun runRcsReadinessPass() {
        val generation = lifecycleGeneration
        val bootstrap = currentBootstrap(generation) ?: return
        if (!refreshConversationIndex(bootstrapLease)) return
        val now = testHooks.currentTimeMillis()
        if (now - lastBootstrapAtMs >= FULL_BOOTSTRAP_INTERVAL_MS && bootstrap.run() == RcsBootstrapResult.COMPLETE) {
            lastBootstrapAtMs = now
        }
        targetedBootstrap.drainNow()
        sweepOwnedConversations(generation)
        nativeRcsSendWork.start()
    }

    private fun offerTargetedBootstrap(googleConversationId: String) {
        targetedBootstrap.offer(googleConversationId)
        requestRcsBootstrap()
    }

    /** Runs one keyed target on the bootstrap executor. Lease or generation loss stops the drain pass. */
    private fun runTargetedBootstrap(googleConversationId: String): RcsBootstrapResult {
        val bootstrap = currentBootstrap(lifecycleGeneration) ?: return RcsBootstrapResult.STOPPED
        return bootstrap.run(googleConversationId)
    }

    /** Refresh is serialized by the bootstrap executor; a pending-event refusal gets one retry, never a loop. */
    private fun refreshConversationIndex(lease: LinkHandleLease?): Boolean {
        val handle = lease?.handle ?: return false
        val refresh = { runCatching { handle.refreshConversationIndex() }.getOrDefault("{\"code\":\"unavailable\"}") }
        val first = refresh()
        if (first.contains("\"code\":\"ok\"")) return true
        if (!first.contains("pending")) return false
        return refresh().contains("\"code\":\"ok\"")
    }

    private fun sweepOwnedConversations(generation: Long) {
        if (!nativeRcsEnabled()) return
        if (!isCurrent(generation)) return
        requestReceiveSweepAfterReady()
    }

    /** The bootstrap is reused per lease so its candidate continuation survives between runs. */
    private fun currentBootstrap(generation: Long): RcsLinkBootstrap? {
        if (!isCurrent(generation) || !nativeRcsEnabled()) return null
        val lease = synchronized(lock) { workerLease } ?: LinkRuntime.handleLease(this) ?: return null
        return synchronized(lock) {
            if (bootstrapLease !== lease) {
                bootstrapLease = lease
                bootstrap = testHooks.bootstrapFactory(applicationContext, lease) {
                    isCurrent(generation) && LinkRuntime.ownsObservationLease(lease)
                }
            }
            checkNotNull(bootstrap)
        }
    }

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

    private fun beginReceiveRun(lease: LinkHandleLease): Long = synchronized(receiveSweepLock) {
        receiveRunGeneration += 1
        receiveRunLease = lease
        pendingReceiveSweep?.cancelled?.set(true)
        pendingReceiveSweep = null
        runningReceiveSweep?.cancelled?.set(true)
        receiveRunGeneration
    }

    private fun finishReceiveRun(lease: LinkHandleLease, runGeneration: Long) = synchronized(receiveSweepLock) {
        if (receiveRunGeneration != runGeneration || receiveRunLease != lease) return@synchronized
        pendingReceiveSweep?.cancelled?.set(true)
        pendingReceiveSweep = null
        runningReceiveSweep?.cancelled?.set(true)
        receiveRunLease = null
    }

    private fun requestReceiveSweepAfterReady() {
        if (!nativeRcsEnabled()) return
        val schedule = synchronized(receiveSweepLock) {
            val lease = receiveRunLease ?: return@synchronized false
            if (!isCurrent(lifecycleGeneration) || !LinkRuntime.ownsObservationLease(lease)) return@synchronized false
            pendingReceiveSweep?.cancelled?.set(true)
            runningReceiveSweep?.cancelled?.set(true)
            pendingReceiveSweep = ReceiveSweepRequest(
                lifecycleGeneration,
                receiveRunGeneration,
                lease,
                AtomicBoolean(false),
            )
            if (receiveSweepDrainScheduled) false else {
                receiveSweepDrainScheduled = true
                true
            }
        }
        if (!schedule) return
        try {
            testHooks.receiveSweepExecutor.execute(::drainReceiveSweeps)
        } catch (_: RuntimeException) {
            synchronized(receiveSweepLock) {
                pendingReceiveSweep?.cancelled?.set(true)
                pendingReceiveSweep = null
                receiveSweepDrainScheduled = false
            }
            recordGoogleMessagesLinkEvent("link", "rcs_receive_sweep_failed", 0)
        }
    }

    private fun drainReceiveSweeps() {
        while (true) {
            val request = synchronized(receiveSweepLock) {
                val next = pendingReceiveSweep
                if (next == null) {
                    runningReceiveSweep = null
                    receiveSweepDrainScheduled = false
                    return
                }
                pendingReceiveSweep = null
                runningReceiveSweep = next
                next
            }
            if (receiveSweepCurrent(request)) {
                runCatching { testHooks.receiveSweep(request.lease, request.cancelled) }
                    .onFailure { recordGoogleMessagesLinkEvent("link", "rcs_receive_sweep_failed", 0) }
            }
            synchronized(receiveSweepLock) {
                if (runningReceiveSweep === request) runningReceiveSweep = null
            }
        }
    }

    private fun receiveSweepCurrent(request: ReceiveSweepRequest): Boolean {
        if (request.cancelled.get() || !isCurrent(request.serviceGeneration)) return false
        if (!LinkRuntime.ownsObservationLease(request.lease)) return false
        return synchronized(receiveSweepLock) {
            !request.cancelled.get() &&
                receiveRunGeneration == request.runGeneration &&
                receiveRunLease == request.lease &&
                runningReceiveSweep === request
        }
    }

    private fun runWithRetry(lease: LinkHandleLease) {
        val handle = lease.handle
        while (isCurrent(lifecycleGeneration)) {
            val beforeRunToken = LinkCoreBridge.observationToken()
            val beforeRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (!LinkCoreBridge.recordObservation(beforeRunToken, lease, applicationContext, beforeRun)) return
            if (beforeRun.state != "linked") return
            requestLiveTypingUpdate(beforeRun)
            publishStatus(beforeRun)
            testHooks.beforeRun()
            val runGeneration = beginReceiveRun(lease)
            val failure = runCatching { handle.run() }.exceptionOrNull()
            finishReceiveRun(lease, runGeneration)
            if (filterReasonCode(failure?.message) == "event_overflow") {
                LinkRuntimeEventSink.record("link", "event_overflow", 0)
            }
            if (!isCurrent(lifecycleGeneration) || filterReasonCode(failure?.message) in STOP_CODES) return
            val afterRunToken = LinkCoreBridge.observationToken()
            val afterRun = runCatching { LinkStatus.parse(handle.status()) }.getOrElse { return }
            if (!LinkCoreBridge.recordObservation(afterRunToken, lease, applicationContext, afterRun)) return
            publishStatus(afterRun)
            if (afterRun.state != "linked") {
                requestLiveTypingUpdate(afterRun)
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
        if (::typingBridge.isInitialized) typingBridge.stop()
        cancelled = true
        synchronized(receiveSweepLock) {
            receiveRunGeneration += 1
            receiveRunLease = null
            pendingReceiveSweep?.cancelled?.set(true)
            pendingReceiveSweep = null
            runningReceiveSweep?.cancelled?.set(true)
        }
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
            runCatching {
                if (!isCurrent(generation)) return@runCatching
                val lease = synchronized(lock) { workerLease }
                    ?: LinkRuntime.handleLease(this)
                    ?: return@runCatching
                val handle = lease.handle
                val token = LinkCoreBridge.observationToken()
                val status = LinkStatus.parse(handle.status())
                if (!LinkCoreBridge.recordObservation(token, lease, applicationContext, status)) return@runCatching
                if (!isCurrent(generation)) return@runCatching
                publishStatusNow(status, generation)
            }.onFailure { recordGoogleMessagesLinkEvent("link", "rcs_callback_failed", 0) }
        }
    }

    /** Persists the observed status before callbacks query lifecycle-gated core state. */
    private fun recordCurrentLifecycleAfterEvent(): Boolean = runCatching {
        val lease = synchronized(lock) { workerLease }
            ?: LinkRuntime.handleLease(this)
            ?: return false
        val status = LinkStatus.parse(lease.handle.status())
        LinkCoreBridge.recordObservation(LinkCoreBridge.observationToken(), lease, applicationContext, status)
    }.getOrDefault(false)

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

    /** Reads Go state before the gateway session, so no core mutex is held across a Go call. */
    private fun requestLiveTypingUpdate(status: LinkStatus? = null) {
        if (!nativeRcsEnabled()) return
        val generation = lifecycleGeneration
        testHooks.typingExecutor.execute {
            if (!isCurrent(generation)) return@execute
            val current = status ?: LinkRuntime.handleLease(this)?.let { lease ->
                runCatching { LinkStatus.parse(lease.handle.status()) }.getOrNull()
            }
            if (!isCurrent(generation)) return@execute
            if (current?.state != "linked" || !current.running) {
                typingBridge.stop()
                return@execute
            }
            val credentials = testHooks.typingSession(applicationContext)
            if (!isCurrent(generation)) return@execute
            if (credentials == null) typingBridge.stop() else typingBridge.start(credentials)
        }
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
        private const val FULL_BOOTSTRAP_INTERVAL_MS = 5 * 60 * 1000L
        private const val MAX_PAIRING_WAIT_MS = 30_000L + 30_000L + 300_000L + 30_000L
        private val STOP_CODES = setOf("link_closed", "unlink_in_progress", "pair_cancelled", "run_refused")
        private val ATTENTION_STATES = setOf("needs_relink", "signed_out")
        private val PRE_START_STATUS = LinkStatus(0, "unknown", false, false, false, 0, "")
        @Volatile private var active: GoogleMessagesLinkService? = null
        @Volatile private var serviceGeneration = 0L
        private val activeLock = Any()
        internal var testHooks = ServiceTestHooks()
        @Volatile internal var nativeRcsSendExecutorEnabledForTest: Boolean? = null
        internal fun nativeRcsEnabled(): Boolean = nativeRcsSendExecutorEnabledForTest ?: NATIVE_RCS_ENABLED

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
        internal fun requestTargetedRcsBootstrap(googleConversationId: String) {
            active?.offerTargetedBootstrap(googleConversationId)
        }
        fun cancelAttention(context: Context) { NotificationManagerCompat.from(context).cancel(NOTIFICATION_ID) }
        internal fun onLinkEvent(kind: String, code: String) {
            active?.let { service ->
                runCatching {
                    val connectionReady = kind == "conn" && code in setOf("ready", "recovered")
                    val indexStale = kind == "index" && code == "stale"
                    val publishesEligibility = connectionReady || indexStale || (kind == "conversation" && code == "updated")
                    val lifecycleRecorded = !publishesEligibility || service.recordCurrentLifecycleAfterEvent()
                    if (connectionReady) {
                        service.backoff.reset()
                    }
                    service.requestLiveTypingUpdate()
                    if (publishesEligibility && lifecycleRecorded) {
                        service.requestRcsBootstrap()
                        if (connectionReady) service.publishNativeRcsEligibility()
                        else if (kind == "conversation" && code == "updated") service.queueNativeRcsEligibility()
                    }
					service.refreshNotificationAfterEvent()
                }.onFailure { recordGoogleMessagesLinkEvent("link", "rcs_callback_failed", 0) }
            }
        }
        internal fun registerSendWake() {
            RcsSendWake.listener = {
                runCatching { active?.nativeRcsSendWork?.start() }
                    .onFailure { recordGoogleMessagesLinkEvent("link", "rcs_callback_failed", 0) }
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
        private const val NATIVE_RCS_ENABLED = false
        private val NATIVE_RCS_SEND_EXECUTOR = Executors.newSingleThreadExecutor()
    }
}
