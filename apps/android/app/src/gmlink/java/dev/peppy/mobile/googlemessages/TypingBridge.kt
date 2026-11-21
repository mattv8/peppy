package dev.peppy.mobile.googlemessages

import android.content.Context
import android.os.SystemClock
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.googlemessages.live.LiveTypingCredentials
import dev.peppy.mobile.googlemessages.live.LiveTypingDisabledReason
import dev.peppy.mobile.googlemessages.live.LiveTypingEnableContext
import dev.peppy.mobile.googlemessages.live.LiveTypingHost
import dev.peppy.mobile.googlemessages.live.LiveTypingListener
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeLiveClock
import uniffi.peppy_mobile_bindings.NativeLiveEnableContext
import uniffi.peppy_mobile_bindings.NativeLiveExecutor
import uniffi.peppy_mobile_bindings.NativeTypingOrigin
import uniffi.peppy_mobile_bindings.NativeTypingState

internal data class TypingCredentials(val origin: String, val bearerToken: String, val localDeviceId: String, val generation: Long)
internal data class TypingReceiveContext(val conversationId: String, val linkOwned: Boolean)

internal interface TypingCore {
    fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock): ULong
    fun setSharing(enabled: Boolean)
    fun disable()
    fun receiveContext(googleConversationId: String): TypingReceiveContext?
    fun ownedGoogleConversationId(conversationId: String): String?
    /** [participant] is the canonical sender of a group typing event; null for one-to-one. */
    fun seal(conversationId: String, gatewayDeviceId: String, participant: String?, state: NativeTypingState, clock: NativeLiveClock): uniffi.peppy_mobile_bindings.NativeLiveOutbound?
    fun open(frameJson: String, clock: NativeLiveClock): uniffi.peppy_mobile_bindings.NativeLiveOutcome
}

internal interface TypingTransport {
    fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener)
    fun stop()
    fun send(frameJson: String): Boolean
    fun send(frameJson: String, publication: Long): Boolean = send(frameJson)
}

private class AndroidTypingTransport(private val host: LiveTypingHost) : TypingTransport {
    override fun start(credentials: LiveTypingCredentials, listener: LiveTypingListener) = host.start(credentials, listener)
    override fun stop() = host.stop()
    override fun send(frameJson: String) = false
    override fun send(frameJson: String, publication: Long) = host.send(frameJson, publication)
}

private class NativeTypingCore(private val client: uniffi.peppy_mobile_bindings.NativeClient) : TypingCore {
    override fun enable(context: NativeLiveEnableContext, clock: NativeLiveClock) = client.enableLiveTyping(context, clock)
    override fun setSharing(enabled: Boolean) = client.setTypingSharing(enabled)
    override fun disable() = client.disableLiveTyping()
    override fun receiveContext(googleConversationId: String) = client.gatewayRcsReceiveContext(googleConversationId)
        ?.let { TypingReceiveContext(it.conversationId, it.owner == uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner.LINK) }
    override fun ownedGoogleConversationId(conversationId: String) = client.gatewayRcsLinkOwnedConversations()
        .firstOrNull { it.conversationId == conversationId && it.owner == uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner.LINK }
        ?.googleConversationId
    override fun seal(conversationId: String, gatewayDeviceId: String, participant: String?, state: NativeTypingState, clock: NativeLiveClock) =
        client.sealTypingSignalV2(conversationId, gatewayDeviceId, NativeTypingOrigin.GOOGLE_CONTACT, participant, state, clock)
    override fun open(frameJson: String, clock: NativeLiveClock) = client.openTypingSignalV2(frameJson, clock)
}

/** Connects the Google link's typed Go callbacks with the memory-only live typing core. */
internal class TypingBridge(
    private val context: Context,
    private val host: TypingTransport = AndroidTypingTransport(LiveTypingHost(context)),
    private val executor: Executor = Executors.newSingleThreadExecutor(),
    private val client: () -> TypingCore? = { NativeGateway.open(context)?.let(::NativeTypingCore) },
    private val sharing: TypingSharingPreferences = TypingSharingPreferences(context),
    private val googleEffect: (LiveTypingEffectIntent) -> String = { intent -> LinkRuntime.setTyping(context, intent) },
) : TypingReceiveSink, LiveTypingListener {
    private val clock = MonotonicClock { SystemClock.elapsedRealtime().toULong() }
    private val adapter = LiveTypingAdapter(clock, executor, ::droppedStale, ::applyOutbound)
    private var wanted = false
    private var localDeviceId: String? = null
    private var sessionGeneration: Long? = null
    private var enabledGeneration: ULong? = null
    private var enabledPublication: Long? = null
    private var fuseAt: ULong? = null
    private var fuseCooldown = INITIAL_FUSE_COOLDOWN_MS
    private var pendingEnable: LiveTypingEnableContext? = null
    private var lastEnableAttempt = 0UL
    private val startedValidity = mutableMapOf<String, ULong>()
    private val incarnation = AtomicLong()

    /** Starts only after the service has observed a linked, running Go session. */
    fun start(session: TypingCredentials) {
        executor.execute {
            if (wanted && sessionGeneration == session.generation) {
                val fused = fuseAt ?: return@execute
                val now = clock.nowMs()
                if (now < fused || now - fused < fuseCooldown) return@execute
                fuseAt = null
                fuseCooldown = minOf(fuseCooldown * 2UL, MAX_FUSE_COOLDOWN_MS)
                recordGoogleMessagesLinkEvent("live", "restart_after_fuse", 0)
            } else {
                fuseAt = null
                fuseCooldown = INITIAL_FUSE_COOLDOWN_MS
                pendingEnable = null
            }
            wanted = true
            localDeviceId = session.localDeviceId
            sessionGeneration = session.generation
            host.start(LiveTypingCredentials(session.origin, session.bearerToken, session.localDeviceId), this)
        }
    }

    fun stop() {
        retireAuthority()
        executor.execute {
            wanted = false
            sessionGeneration = null
            fuseAt = null
            fuseCooldown = INITIAL_FUSE_COOLDOWN_MS
            pendingEnable = null
            disableCore()
            host.stop()
        }
    }

    override fun onEnabled(context: LiveTypingEnableContext) {
        val callbackIncarnation = incarnation.get()
        executor.execute {
            if (incarnation.get() == callbackIncarnation) enable(context)
        }
    }

    private fun enable(context: LiveTypingEnableContext) {
        if (!wanted || context.localDeviceId != localDeviceId) return
        pendingEnable = context
        lastEnableAttempt = clock.nowMs()
        val current = client() ?: run {
            recordGoogleMessagesLinkEvent("typing", "enable_no_client", 0)
            disableCore()
            return
        }
        runCatching {
            current.enable(
                NativeLiveEnableContext(context.rosterIds, context.localDeviceId, NativeLiveExecutor.CURRENT_GATEWAY),
                nativeClock(),
            )
        }.onSuccess { generation ->
            if (!sharing.reapplyAfterEnable { enabled -> current.setSharing(enabled) }) {
                recordGoogleMessagesLinkEvent("typing", "enable_sharing_failed", 0)
                disableCore()
                return@onSuccess
            }
            adapter.activate(generation)
            enabledGeneration = generation
            enabledPublication = context.publication
            pendingEnable = null
            recordGoogleMessagesLinkEvent("typing", "enabled", 0)
        }.onFailure {
            recordGoogleMessagesLinkEvent("typing", "enable_failed", 0)
            disableCore()
        }
    }

    override fun onMembershipRenewed() {
        val callbackIncarnation = incarnation.get()
        executor.execute {
            if (incarnation.get() != callbackIncarnation) return@execute
            val context = pendingEnable ?: return@execute
            val now = clock.nowMs()
            if (now < lastEnableAttempt || now - lastEnableAttempt < ENABLE_RETRY_MS) return@execute
            recordGoogleMessagesLinkEvent("typing", "enable_retry", 0)
            enable(context)
        }
    }

    override fun onDisabled(reason: LiveTypingDisabledReason) {
        retireAuthority()
        executor.execute {
            pendingEnable = null
            if (wanted && reason == LiveTypingDisabledReason.PROTOCOL_ERROR) {
                fuseAt = clock.nowMs()
            }
            disableCore()
        }
    }

    override fun onFrame(json: String) {
        val frameIncarnation = incarnation.get()
        executor.execute {
            val current = client() ?: return@execute recordGoogleMessagesLinkEvent("typing", "open_failed", 0)
            runCatching { current.open(json, nativeClock()) }
                .onSuccess { outcome ->
                    var canSendFollowUps = true
                    outcome.replyFrameJson?.let { reply ->
                        when (sendIfCurrent(reply, outcome.generation, frameIncarnation)) {
                            SendResult.SENT -> Unit
                            SendResult.NOT_CURRENT -> {
                                canSendFollowUps = false
                                recordGoogleMessagesLinkEvent("typing", "reply_not_current", 0)
                            }
                            SendResult.REFUSED -> {
                                canSendFollowUps = false
                                recordGoogleMessagesLinkEvent("live", "send_refused", 0)
                            }
                        }
                    }
                    var replayed = false
                    if (canSendFollowUps) {
                        for (frame in outcome.followUpFramesJson) {
                            when (sendIfCurrent(frame, outcome.generation, frameIncarnation)) {
                                SendResult.SENT -> replayed = true
                                SendResult.NOT_CURRENT -> {
                                    recordGoogleMessagesLinkEvent("typing", "reply_not_current", 0)
                                    break
                                }
                                SendResult.REFUSED -> {
                                    recordGoogleMessagesLinkEvent("live", "send_refused", 0)
                                    break
                                }
                            }
                        }
                    }
                    if (replayed) recordGoogleMessagesLinkEvent("typing", "replayed", 0)
                    adapter.consume(outcome) { isCurrent(outcome.generation, frameIncarnation) }
                }
                .onFailure { recordGoogleMessagesLinkEvent("typing", "open_failed", 0) }
        }
    }

    /** Go has already projected this to a one-to-one Google conversation; core confirms ownership. */
    override fun receive(recordJson: String) {
        val frameIncarnation = incarnation.get()
        executor.execute {
            val current = client() ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_no_client", 0)
            val deviceId = localDeviceId ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_no_client", 0)
            val record = runCatching { JSONObject(recordJson) }.getOrNull()
                ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_malformed", 0)
            val googleId = record.optString("google_conversation_id").takeIf(String::isNotEmpty)
                ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_malformed", 0)
            val state = when (record.optString("state")) {
                "started" -> NativeTypingState.STARTED
                "stopped" -> NativeTypingState.STOPPED
                else -> return@execute recordGoogleMessagesLinkEvent("typing", "inbound_malformed", 0)
            }
            val receive = runCatching { current.receiveContext(googleId) }.getOrNull()
                ?.takeIf { it.linkOwned }
                ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_not_owned", 0)
            // Go sends `participant` only for a group conversation; core confirms it is a live roster member.
            val participant = record.optString("participant").takeIf(String::isNotEmpty)
            val result = runCatching {
                current.seal(
                    receive.conversationId,
                    deviceId,
                    participant,
                    state,
                    nativeClock(),
                )
            }
            if (result.isFailure) return@execute recordGoogleMessagesLinkEvent("typing", "inbound_seal_failed", 0)
            val frame = result.getOrNull()
                ?: return@execute recordGoogleMessagesLinkEvent("typing", "inbound_seal_refused", 0)
            when (sendIfCurrent(frame.frameJson, frame.generation, frameIncarnation)) {
                SendResult.SENT -> recordGoogleMessagesLinkEvent("typing", "inbound_sealed", 0)
                SendResult.NOT_CURRENT -> recordGoogleMessagesLinkEvent("typing", "inbound_not_current", 0)
                SendResult.REFUSED -> recordGoogleMessagesLinkEvent("live", "send_refused", 0)
            }
        }
    }

    private fun applyOutbound(intent: LiveTypingEffectIntent) {
        val googleConversationId = runCatching { client()?.ownedGoogleConversationId(intent.conversationId) }.getOrNull()
            ?: return recordGoogleMessagesLinkEvent("typing", "outbound_not_owned", 0)
        val now = clock.nowMs()
        if (startedValidity[googleConversationId]?.let { it > now } == true) {
            recordGoogleMessagesLinkEvent("typing", "suppressed", 0)
            return
        }
        val result = googleEffect(intent.copy(conversationId = googleConversationId))
        if (result == "ok") {
            startedValidity[googleConversationId] = minOf(intent.deadlineMono, now + MAX_GOOGLE_TYPING_VALIDITY_MS)
            recordGoogleMessagesLinkEvent("typing", "outbound_effect", 0)
        } else {
            val reason = when (result) {
                "typing_unavailable" -> "refused_unavailable"
                "typing_stale" -> "refused_stale"
                "typing_expired", "typing_deadline" -> "refused_expired"
                "typing_failed", "typing_cancelled" -> "refused_failed"
                "typing_invalid", "typing_stopped" -> "refused_invalid"
                "typing_refused", "typing_unknown_conversation", "typing_not_one_to_one" -> "refused_conversation"
                else -> "outbound_refused"
            }
            recordGoogleMessagesLinkEvent("typing", reason, 0)
        }
    }

    private enum class SendResult { SENT, NOT_CURRENT, REFUSED }

    private fun sendIfCurrent(frameJson: String, generation: ULong, frameIncarnation: Long): SendResult {
        val publication = enabledPublication
        if (publication == null || !isCurrent(generation, frameIncarnation)) return SendResult.NOT_CURRENT
        return if (host.send(frameJson, publication)) SendResult.SENT else SendResult.REFUSED
    }

    private fun isCurrent(generation: ULong, frameIncarnation: Long): Boolean =
        enabledGeneration == generation && incarnation.get() == frameIncarnation

    private fun retireAuthority() {
        incarnation.incrementAndGet()
    }

    private fun disableCore() {
        enabledGeneration = null
        enabledPublication = null
        startedValidity.clear()
        adapter.disable()
        runCatching { client()?.disable() }
        recordGoogleMessagesLinkEvent("typing", "disabled", 0)
    }

    private fun droppedStale() = recordGoogleMessagesLinkEvent("typing", "dropped_stale", 0)

    private fun nativeClock() = NativeLiveClock(System.currentTimeMillis(), clock.nowMs())

    private companion object {
        const val MAX_GOOGLE_TYPING_VALIDITY_MS = 5_000UL
        const val INITIAL_FUSE_COOLDOWN_MS = 300_000UL
        const val MAX_FUSE_COOLDOWN_MS = 3_600_000UL
        const val ENABLE_RETRY_MS = 60_000UL
    }
}

/** The link can be opened before the service; inactive links deliberately discard Go typing callbacks. */
internal object TypingReceiveSinkRegistry {
    @Volatile var sink: TypingReceiveSink? = null
}
