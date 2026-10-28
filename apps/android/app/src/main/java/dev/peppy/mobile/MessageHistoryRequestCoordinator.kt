package dev.peppy.mobile

import org.json.JSONException
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMessageHistorySourceFacts

/** Android's thin adapter around the core-owned history capture admission lifecycle. */
internal class MessageHistoryRequestCoordinator(
    private val client: NativeClientInterface,
    private val onControlPublished: () -> Unit = {},
) {
    data class Admission(val token: String, val budget: Int)
    data class Readiness(val needsSms: Boolean, val needsMms: Boolean)

    fun publish(generation: String, sms: Boolean, mms: Boolean, permission: Boolean, policy: Boolean) {
        synchronized(publicationLock) {
            if (publishedClient !== client) {
                publishedClient = client
                forcePublication = true
            }
            client.publishMessageHistoryCaptureSource(
                NativeMessageHistorySourceFacts(generation, sms, mms, permission, policy, forcePublication),
            )
            forcePublication = false
        }
    }

    fun pending(generation: String): Readiness {
        val json = JSONObject(client.pendingMessageHistoryCaptureRequests(generation))
        return Readiness(json.requiredBoolean("needsSms"), json.requiredBoolean("needsMms"))
    }

    fun begin(generation: String, transport: String): Admission? {
        val value = client.beginMessageHistoryCapture(generation, transport)
        if (value.trim() == "null") return null
        val json = JSONObject(value)
        val token = json.getString("token")
        val budget = json.getInt("budget")
        require(token.isNotBlank()) { "history admission token is blank" }
        require(budget in 1..MAX_OBSERVATIONS) { "history admission budget is out of range" }
        return Admission(token, budget)
    }

    fun finish(generation: String, admission: Admission, observations: Int, exhausted: Boolean, reason: String? = null) {
        require(observations in 0..MAX_OBSERVATIONS) { "history observations are out of range" }
        client.finishMessageHistoryCapture(
            JSONObject()
                .put("providerGeneration", generation)
                .put("token", admission.token)
                .put("observations", observations)
                .put("exhausted", exhausted)
                .put("reason", reason)
                .toString(),
        )
        runCatching { drain(generation) }
            .getOrNull()
            ?.takeIf { it > 0 }
            ?.let { onControlPublished() }
    }

    fun drain(generation: String): Int {
        val published = JSONObject(client.drainMessageHistoryCaptureResults(generation)).getInt("published")
        require(published >= 0) { "published history result count is negative" }
        return published
    }

    private fun JSONObject.requiredBoolean(name: String): Boolean =
        opt(name) as? Boolean ?: throw JSONException(name)

    companion object {
        private const val MAX_OBSERVATIONS = 100
        private val publicationLock = Any()
        private var publishedClient: NativeClientInterface? = null
        private var forcePublication = true
    }
}

internal interface MmsHistoryCaptureAdmission {
    fun begin(): MessageHistoryRequestCoordinator.Admission?
    fun finish(admission: MessageHistoryRequestCoordinator.Admission, observations: Int, exhausted: Boolean, reason: String? = null)
}
