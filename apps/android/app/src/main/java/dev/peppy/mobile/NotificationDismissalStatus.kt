package dev.peppy.mobile

import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeGatewayHostFacts
import uniffi.peppy_mobile_bindings.NativeGatewayPlatform

/** Core-owned publish: the core decides whether a status event is due. */
internal interface NotificationDismissalStatusCore {
    fun publish(platform: NativeGatewayPlatform, facts: NativeGatewayHostFacts, nowMs: Long): Boolean
}

internal class NativeNotificationDismissalStatusCore(private val client: NativeClientInterface) : NotificationDismissalStatusCore {
    override fun publish(platform: NativeGatewayPlatform, facts: NativeGatewayHostFacts, nowMs: Long) =
        client.publishNotificationDismissalStatus(platform, facts, nowMs)
}

/**
 * Tells the computer whether this phone can dismiss mirrored notifications. It needs no listener
 * connection: the status comes from OS access and settings, not from a posted notification.
 */
internal class NotificationDismissalStatus(
    private val core: NotificationDismissalStatusCore,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    /**
     * Returns whether a status event was queued. Locked keys or a closed client read as not queued:
     * both are expected states, and the next pass tries again. Any other binding failure propagates,
     * so the gateway phase records it like the other core phases.
     */
    fun publish(facts: NativeGatewayHostFacts): Boolean =
        try {
            core.publish(NativeGatewayPlatform.ANDROID, facts, nowMs())
        } catch (_: MobileBindingsException.KeysUnavailable) {
            false
        } catch (_: MobileBindingsException.Closed) {
            false
        }
}
