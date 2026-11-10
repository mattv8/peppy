package dev.peppy.mobile.googlemessages

import uniffi.peppy_mobile_bindings.NativeLiveAction
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeTypingState
import java.util.concurrent.Executor

internal fun interface MonotonicClock {
    fun nowMs(): ULong
}

internal data class LiveTypingEffectIntent(
    val conversationId: String,
    val state: NativeTypingState,
    val deadlineMono: ULong,
    val clock: MonotonicClock,
    val isCurrent: LiveTypingCurrency,
)

internal fun interface LiveTypingCurrency {
    fun isCurrent(): Boolean
}

internal fun interface LiveTypingEffect {
    fun setTyping(intent: LiveTypingEffectIntent)
}

/**
 * Admits only typed core gateway typing intents and drops work retired by the
 * current core generation or peer session. This does not parse incoming frames.
 */
internal class LiveTypingAdapter(
    private val clock: MonotonicClock,
    private val callbackExecutor: Executor,
    private val droppedStale: () -> Unit = {},
    private val effect: LiveTypingEffect,
) {
    private var generation: ULong? = null
    private val sessions = mutableMapOf<String, Session>()
    private val sequences = mutableMapOf<SequenceKey, ULong>()

    @Synchronized
    fun activate(generation: ULong) {
        this.generation = generation
        sessions.clear()
        sequences.clear()
    }

    @Synchronized
    fun disable() {
        generation = null
        sessions.clear()
        sequences.clear()
    }

    fun consume(outcome: NativeLiveOutcome, isAuthorityCurrent: () -> Boolean = { true }) {
        val admitted = synchronized(this) {
            if (outcome.generation != generation || !isAuthorityCurrent()) return@synchronized null

            outcome.sessionUpdate?.let { update ->
                val current = sessions[update.peerDeviceId]
                if (current == null || update.validationRevision > current.revision) {
                    sessions[update.peerDeviceId] = Session(update.currentSessionToken, update.validationRevision)
                    sequences.keys.removeAll { it.peerDeviceId == update.peerDeviceId }
                }
            }

            val action = outcome.action as? NativeLiveAction.GatewayTypingIntent ?: return@synchronized null
            val session = sessions[action.peerDeviceId]
                ?.takeIf { it.token == action.sessionToken }
                ?: return@synchronized null

            val sequenceKey = SequenceKey(action.peerDeviceId, action.conversationId, session)
            val previous = sequences[sequenceKey]
            if (previous != null && action.sequence <= previous) return@synchronized null
            sequences[sequenceKey] = action.sequence
            if (action.state != NativeTypingState.STARTED || action.deadlineMono <= clock.nowMs()) return@synchronized null
            AdmittedIntent(outcome.generation, sequenceKey, action.sequence, action.deadlineMono, isAuthorityCurrent)
        } ?: return droppedStale()

        callbackExecutor.execute {
            publishIfCurrent(admitted)
        }
    }

    private fun publishIfCurrent(admitted: AdmittedIntent) {
        // Runtime admission takes its native-operation lock before querying this currency.
        // Never call the effect under this monitor, so adapter updates cannot wait on that lock.
        val intent = synchronized(this) {
            if (!isCurrent(admitted) || !admitted.isAuthorityCurrent() || admitted.deadlineMono <= clock.nowMs()) {
                droppedStale()
                return
            }
            LiveTypingEffectIntent(
                admitted.key.conversationId,
                NativeTypingState.STARTED,
                admitted.deadlineMono,
                clock,
                // Re-checked by LinkRuntime under its native lock, after the Google-ID lookup may have run.
                LiveTypingCurrency { isCurrent(admitted) && admitted.isAuthorityCurrent() },
            )
        }
        effect.setTyping(intent)
    }

    private fun isCurrent(admitted: AdmittedIntent): Boolean = synchronized(this) {
        generation == admitted.generation &&
            sessions[admitted.key.peerDeviceId] == admitted.key.session &&
            sequences[admitted.key] == admitted.sequence
    }

    private data class Session(val token: ULong, val revision: ULong)

    private data class SequenceKey(
        val peerDeviceId: String,
        val conversationId: String,
        val session: Session,
    )

    private data class AdmittedIntent(
        val generation: ULong,
        val key: SequenceKey,
        val sequence: ULong,
        val deadlineMono: ULong,
        val isAuthorityCurrent: () -> Boolean,
    )

}
