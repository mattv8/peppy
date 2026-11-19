package dev.peppy.mobile.gateway

import android.content.Context
import uniffi.peppy_mobile_bindings.NativeClientInterface

/** Called on the gateway or native RCS worker, never on the main thread. */
internal fun retireRevokedProducers(context: Context, client: NativeClientInterface): Boolean {
    val account = GatewayAccountHost.load(context) ?: return false
    client.retireUnattemptedCommandsFromProducers(account.devices.filter { it.revoked }.map { it.id })
    return true
}

internal fun cachedProducerRetirement(
    load: () -> List<String>?,
    retire: (List<String>) -> Unit,
): () -> Boolean {
    var loaded = false
    var revoked: List<String>? = null
    return {
        if (!loaded) {
            revoked = load()
            loaded = true
        }
        val ids = revoked
        if (ids == null) false else {
            retire(ids)
            true
        }
    }
}

internal fun cachedProducerRetirement(context: Context, client: NativeClientInterface): () -> Boolean =
    cachedProducerRetirement(
        load = { GatewayAccountHost.load(context)?.devices?.filter { it.revoked }?.map { it.id } },
        retire = { client.retireUnattemptedCommandsFromProducers(it) },
    )

/** Re-read after retirement so a candidate captured before the fence cannot reach admission. */
internal fun <T> revocationCheckedCandidates(read: () -> List<T>, retire: () -> Boolean): List<T> {
    val pending = read()
    if (pending.isEmpty() || !retire()) return pending
    return read()
}
