package dev.peppy.mobile.sms

import dev.peppy.mobile.diagnostics.SyncDiagnosticReason
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import dev.peppy.mobile.diagnostics.SyncTraceField
import dev.peppy.mobile.diagnostics.recordGoogleMessagesLinkEvent
import uniffi.peppy_mobile_bindings.NativeRcsProviderRoute
import uniffi.peppy_mobile_bindings.NativeRcsProviderRouteReason
import uniffi.peppy_mobile_bindings.NativeRcsProviderRowRoute
import uniffi.peppy_mobile_bindings.MobileBindingsException

internal enum class ProviderAcquisitionRoute { ACQUIRE, SKIP, DEFER }

/** Routes provider rows while local link state exists; malformed rows remain ordinary provider input. */
internal class RcsProviderRowRouter(
    private val routeJson: (String) -> NativeRcsProviderRoute,
) {
    private var enabled = true

    fun route(
        providerRowId: Long,
        providerThreadId: Long?,
        providerType: Int,
        observedAtMs: Long?,
        addresses: List<String>,
        restricted: Boolean = false,
        textOnly: Boolean = false,
        transactionId: String? = null,
    ): ProviderAcquisitionRoute {
        val routableAddresses = addresses.map { it.trim() }.filter { it.isNotEmpty() }
        if (!enabled || providerRowId < 0 || (providerThreadId ?: 0L) < 0 || providerType < 0 ||
            routableAddresses.isEmpty() || routableAddresses.size > MAX_ADDRESSES ||
            routableAddresses.any { it.toByteArray(Charsets.UTF_8).size > MAX_ADDRESS_BYTES }) {
            return ProviderAcquisitionRoute.ACQUIRE
        }
        return try {
            val result = routeJson(providerRowJson(providerRowId, providerThreadId, providerType, observedAtMs, routableAddresses, restricted, textOnly, transactionId))
            if (restricted) recordLinkOutcome(result)
            when (result.reason) {
                NativeRcsProviderRouteReason.PRE_PROOF -> diagnostic(SyncDiagnosticReason.RCS_PROVIDER_PRE_PROOF)
                NativeRcsProviderRouteReason.GUARD_BAND -> diagnostic(SyncDiagnosticReason.RCS_PROVIDER_GUARD_BAND)
                else -> Unit
            }
            when (result.route) {
                NativeRcsProviderRowRoute.DETECTOR_RECORDED -> ProviderAcquisitionRoute.SKIP
                NativeRcsProviderRowRoute.DEFER -> ProviderAcquisitionRoute.DEFER
                NativeRcsProviderRowRoute.PROVIDER -> ProviderAcquisitionRoute.ACQUIRE
                NativeRcsProviderRowRoute.ALERT -> {
                    diagnostic(SyncDiagnosticReason.RCS_PROVIDER_ALERT)
                    ProviderAcquisitionRoute.ACQUIRE
                }
            }
        } catch (_: MobileBindingsException.InvalidRequest) {
            // Generated bindings preserve the InvalidRequest class but not the Rust reason. The Kotlin
            // checks above exclude every provider-row validation failure, leaving the core's no-link probe.
            enabled = false
            diagnostic(SyncDiagnosticReason.RCS_PROVIDER_ROUTE_ERROR)
            ProviderAcquisitionRoute.ACQUIRE
        } catch (_: Exception) {
            diagnostic(SyncDiagnosticReason.RCS_PROVIDER_ROUTE_ERROR)
            ProviderAcquisitionRoute.ACQUIRE
        }
    }

    private fun providerRowJson(
        providerRowId: Long,
        providerThreadId: Long?,
        providerType: Int,
        observedAtMs: Long?,
        addresses: List<String>,
        restricted: Boolean,
        textOnly: Boolean,
        transactionId: String?,
    ): String = buildString {
        append("{\"providerRowId\":").append(providerRowId)
        append(",\"providerThreadId\":").append(providerThreadId ?: 0L)
        append(",\"providerType\":").append(providerType)
        append(",\"observedAtMs\":").append(observedAtMs ?: 0L)
        append(",\"restricted\":").append(restricted)
        append(",\"textOnly\":").append(textOnly)
        append(",\"addresses\":[")
        addresses.forEachIndexed { index, address ->
            if (index > 0) append(',')
            append('"').append(jsonString(address)).append('"')
        }
        append(']')
        transactionId?.takeIf { it.isNotBlank() && it.toByteArray(Charsets.UTF_8).size <= MAX_TRANSACTION_ID_BYTES }?.let {
            append(",\"transactionId\":\"").append(jsonString(it)).append('"')
        }
        append('}')
    }

    private fun jsonString(value: String): String = buildString {
        value.forEach { character ->
            when (character) {
                '\\' -> append("\\\\")
                '"' -> append("\\\"")
                '\b' -> append("\\b")
                '\u000C' -> append("\\f")
                '\n' -> append("\\n")
                '\r' -> append("\\r")
                '\t' -> append("\\t")
                else -> if (character.code < 0x20) append("\\u%04x".format(character.code)) else append(character)
            }
        }
    }

    /** Which side takes a mirrored chat row: the link, or this capture, and why. */
    private fun recordLinkOutcome(result: NativeRcsProviderRoute) {
        val outcome = when (result.route) {
            NativeRcsProviderRowRoute.ALERT -> "alert"
            NativeRcsProviderRowRoute.DETECTOR_RECORDED -> "left_to_link"
            NativeRcsProviderRowRoute.DEFER -> "deferred"
            NativeRcsProviderRowRoute.PROVIDER -> when (result.reason) {
                NativeRcsProviderRouteReason.PRE_PROOF -> "pre_proof"
                NativeRcsProviderRouteReason.GUARD_BAND -> "guard_band"
                NativeRcsProviderRouteReason.NOT_LINK_OWNED -> "not_link_owned"
                NativeRcsProviderRouteReason.LINK_LATE -> "link_late"
                NativeRcsProviderRouteReason.UNIDENTIFIED -> "unidentified"
                NativeRcsProviderRouteReason.MEDIA -> "media"
                NativeRcsProviderRouteReason.NOT_RESTRICTED_TEXT, null -> return
            }
        }
        recordGoogleMessagesLinkEvent("provider_route", outcome, 0L)
    }

    private fun diagnostic(reason: Long) {
        SyncDiagnostics.record(SyncTraceEvent.NATIVE_CALL, SyncTraceField.REASON to reason)
    }

    companion object {
        private const val MAX_ADDRESSES = 100
        private const val MAX_ADDRESS_BYTES = 512
        private const val MAX_TRANSACTION_ID_BYTES = 512

        fun acquireAll() = RcsProviderRowRouter { NativeRcsProviderRoute(NativeRcsProviderRowRoute.PROVIDER, null) }
    }
}
