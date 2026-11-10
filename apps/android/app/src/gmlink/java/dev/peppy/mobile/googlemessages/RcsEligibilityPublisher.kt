package dev.peppy.mobile.googlemessages

import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeRcsEligibilityPublication
import uniffi.peppy_mobile_bindings.gatewayPublishRcsEligibility

/** Kotlin seam around the narrow core-owned eligibility derivation input. */
internal interface RcsEligibilityPublisherPort {
    fun publish(publications: List<NativeRcsEligibilityPublication>, nowMs: Long): UInt
}

/** Publishes only while its service generation remains current. */
internal class RcsEligibilityPublisher(
    private val port: RcsEligibilityPublisherPort,
    private val current: () -> Boolean,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    fun publish(conversationIds: Collection<String>) {
        if (!current() || conversationIds.isEmpty()) return
        conversationIds.distinct().chunked(MAX_PUBLICATIONS_PER_CALL).forEach { batch ->
            if (!current()) return
            port.publish(batch.map(::NativeRcsEligibilityPublication), nowMs())
        }
    }

    private companion object {
        const val MAX_PUBLICATIONS_PER_CALL = 100
    }
}

/** Captures one native client; callers must invoke this only from a background worker. */
internal class GeneratedRcsEligibilityPublisherPort(
    private val client: NativeClient,
) : RcsEligibilityPublisherPort {
    override fun publish(publications: List<NativeRcsEligibilityPublication>, nowMs: Long): UInt =
        runCatching { gatewayPublishRcsEligibility(client, publications, nowMs) }.getOrDefault(0u)
}
