package dev.peppy.mobile.googlemessages

import org.json.JSONException
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeTypingState

interface LinkHost {
    fun persistState(state: ByteArray)
    fun event(kind: String, code: String, value: Long)
    fun rcsEvent(recordJson: String)
    fun typingEvent(recordJson: String)
    fun showEmoji(emoji: String)
    fun eventOwned(kind: String, code: String, value: Long, generation: Long, revision: Long) = event(kind, code, value)
    fun rcsEventOwned(recordJson: String, generation: Long, revision: Long, streamEpoch: Long) = rcsEvent(recordJson)
    fun typingEventOwned(recordJson: String, generation: Long, revision: Long, streamEpoch: Long) = typingEvent(recordJson)
    fun showEmojiOwned(emoji: String, generation: Long, revision: Long) = showEmoji(emoji)
}

interface LinkHandle {
    fun pair(cookiesJson: ByteArray)
    fun relink()
    fun run()
    fun checkHealth(): Long
    fun unlink()
    fun stop()
    fun close()
    fun status(): String
    fun setTyping(request: LinkTypingRequest): String = "typing_unavailable"
    fun acceptsOutput(generation: Long, revision: Long): Boolean = true
    fun acceptsStreamOutput(generation: Long, revision: Long, streamEpoch: Long): Boolean =
        acceptsOutput(generation, revision)
}

data class LinkTypingRequest(
    val conversationId: String,
    val state: NativeTypingState,
    val remainingValidityMs: ULong,
    val linkGeneration: Long,
) {
    fun toJson(): String = JSONObject()
        .put("conversation_id", conversationId)
        .put("state", if (state == NativeTypingState.STARTED) "started" else "stopped")
        .put("remaining_validity_ms", remainingValidityMs.toLong())
        .put("link_generation", linkGeneration)
        .toString()
}

interface LinkEngine {
    val available: Boolean
    fun open(state: ByteArray, host: LinkHost): LinkHandle
}

data class LinkStatus(
    val seq: Long,
    val state: String,
    val running: Boolean,
    val healthy: Boolean,
    val phoneResponding: Boolean,
    val lastOkMs: Long,
    val stopCode: String,
) {
    companion object {
        private val states = setOf("unlinked", "pairing", "linked", "needs_relink", "signed_out")
        private val codePattern = Regex("[a-z][a-z0-9_]{0,39}")

        fun parse(json: String): LinkStatus {
            return try {
                val value = JSONObject(json)
                val parsedState = value.optString("state")
                // Unknown states and malformed JSON fail closed as unknown; the panel treats this as a load failure.
                val state = parsedState.takeIf(states::contains) ?: "unknown"
                val rawStopCode = value.optString("stop_code")
                val stopCode = if (rawStopCode.isEmpty() || codePattern.matches(rawStopCode)) rawStopCode else "unknown"
                LinkStatus(
                    seq = value.optLong("seq"),
                    state = state,
                    running = value.optBoolean("running"),
                    healthy = value.optBoolean("healthy"),
                    phoneResponding = value.optBoolean("phone_responding"),
                    lastOkMs = value.optLong("last_ok_ms"),
                    stopCode = stopCode,
                )
            } catch (_: JSONException) {
                LinkStatus(0, "unknown", false, false, false, 0, "unknown")
            }
        }
    }
}

sealed interface LinkOpenResult {
    data class Open(val handle: LinkHandle) : LinkOpenResult
    data object Unavailable : LinkOpenResult
    data class Unreadable(val code: String) : LinkOpenResult
}


internal fun interface RcsReceiveSink {
    fun receive(recordJson: String)
}

internal object NoOpRcsReceiveSink : RcsReceiveSink {
    override fun receive(recordJson: String) = Unit
}


internal fun interface TypingReceiveSink {
    fun receive(recordJson: String)
}

internal object NoOpTypingReceiveSink : TypingReceiveSink {
    override fun receive(recordJson: String) = Unit
}
