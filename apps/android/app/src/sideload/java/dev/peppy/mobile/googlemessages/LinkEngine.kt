package dev.peppy.mobile.googlemessages

import org.json.JSONObject

interface LinkHost {
    fun persistState(state: ByteArray)
    fun event(kind: String, code: String, value: Long)
    fun showEmoji(emoji: String)
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
}

interface LinkEngine {
    val available: Boolean
    fun open(state: ByteArray, host: LinkHost): LinkHandle
}

data class LinkStatus(
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
            val value = JSONObject(json)
            val parsedState = value.optString("state")
            // Unknown states fail closed as unknown; the panel treats this as a load failure.
            val state = parsedState.takeIf(states::contains) ?: "unknown"
            val stopCode = value.optString("stop_code").takeIf(codePattern::matches) ?: "unknown"
            return LinkStatus(
                state = state,
                running = value.optBoolean("running"),
                healthy = value.optBoolean("healthy"),
                phoneResponding = value.optBoolean("phone_responding"),
                lastOkMs = value.optLong("last_ok_ms"),
                stopCode = stopCode,
            )
        }
    }
}

sealed interface LinkOpenResult {
    data class Open(val handle: LinkHandle) : LinkOpenResult
    data object Unavailable : LinkOpenResult
    data class Unreadable(val code: String) : LinkOpenResult
}
