package dev.peppy.mobile.restrictedsetup

/**
 * `close` is idempotent, aborts outstanding I/O, and permanently closes this instance.
 * Every suspend operation must throw after `close`; implementations must never reopen it.
 */
internal interface RestrictedSetupTransport : AutoCloseable {
    suspend fun pair(endpoint: LocalEndpoint, code: CharArray)
    suspend fun connect(endpoint: LocalEndpoint)
    suspend fun grantAdvancedFeatures()
    suspend fun turnWirelessDebuggingOff()

    override fun close()
}

internal class PairingRejectedException : Exception()

internal fun interface RestrictedSetupTransportFactory {
    fun create(): RestrictedSetupTransport
}
