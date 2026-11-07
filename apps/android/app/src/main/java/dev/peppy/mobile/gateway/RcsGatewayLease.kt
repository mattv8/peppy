package dev.peppy.mobile.gateway

import uniffi.peppy_mobile_bindings.NativeClient

/** Ticket-bound ownership of one NativeGateway generation. */
internal class RcsGatewayLease internal constructor(
    internal val client: NativeClient,
    internal val generation: Long,
    private val release: () -> Unit,
) : AutoCloseable {
    private var closed = false

    override fun close() {
        if (closed) return
        closed = true
        release()
    }
}
