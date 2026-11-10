package dev.peppy.mobile.googlemessages

import android.util.JsonReader
import android.util.JsonToken
import java.io.StringReader
import java.util.UUID
import org.json.JSONObject

/** Strict, ticket-bound Kotlin port of the native Go RCS operation. */
internal class RcsNativeGoAdapter(private val lease: LinkHandleLease) : RcsNativeGoOperation {
    override fun ownerGeneration(): Long = LinkRuntime.withCapturedLinkOperation(lease) { it.ownerGeneration() } ?: 0

    override fun isCurrent(): Boolean = LinkRuntime.ownsObservationLease(lease)

    override fun prepare(command: RcsNativeCommand, canonicalizerRevision: Int, route: RcsNativeRouteEvidence, expectedMemberDigest: String?): RcsNativePrepared {
        val request = JSONObject()
            .put("command_id", command.commandId)
            .put("canonicalizer_revision", canonicalizerRevision)
            .put("identity_region", route.identityRegion)
            .put("carrier_binding", route.carrierBindingJson())
            .apply { expectedMemberDigest?.let { put("expected_member_digest", it) } }
            .toString()
        val response = scoped { it.prepareNativeRcs(request) }
        // Go refuses a group prepare before any ticket exists, so there is nothing to revoke.
        if (membersChangedOnly(response)) throw RcsNativeMembersChanged()
        var ticketForRevocation: String? = null
        // A response rejected before decoding has no trusted ticket id. Go has no ticket-less revoke, so that slot expires by its 30 s TTL.
        return try {
            val parsed = strictParse(
                response,
                PREPARE_CAP,
                setOf("code", "ticket_id", "candidate_snapshot", "run_incarnation", "pairing_evidence_id"),
                setOf("run_generation", "stream_epoch"),
            ) { ticketForRevocation = validatedTicketId(it) }
            if (parsed["code"] != "prepared") throw NativeRcsRefusal()
            val ticket = parsed.string("ticket_id") ?: throw NativeRcsRefusal()
            val candidate = parsed.string("candidate_snapshot")?.takeIf(::withinCandidateCap) ?: throw NativeRcsRefusal()
            RcsNativePrepared(ticket, candidate, parsed.string("run_incarnation") ?: throw NativeRcsRefusal(), parsed.uint64("run_generation") ?: throw NativeRcsRefusal(), parsed.uint64("stream_epoch") ?: throw NativeRcsRefusal(), parsed.string("pairing_evidence_id") ?: throw NativeRcsRefusal())
        } catch (failure: Throwable) {
            preserveInterruption(failure)
            ticketForRevocation?.let(::revoke)
            throw failure
        }
    }

    override fun bindRoute(ticketId: String, tuple: RcsNativeContextTuple, mappingRevision: ULong, observedAtMs: Long): String {
        val values = listOf(tuple.linkGeneration, tuple.sessionGeneration, tuple.contextRevision, mappingRevision).map { it.toSignedLong() ?: throw NativeRcsRefusal() }
        return observation(scoped { it.bindNativeRcsRoute(ticketId, values[0], values[1], values[2], values[3], observedAtMs) }, "route_bound")
    }

    override fun classifyTarget(ticketId: String, classify: (String) -> RcsNativeTarget): RcsNativeTarget? {
        var result: RcsNativeTarget? = null
        val completed = try {
            LinkRuntime.withCapturedLinkOperation(lease) { handle ->
                handle.withNativeRcsTargetClassification(ticketId, NativeRcsTargetClassifier { snapshot ->
                    try {
                        result = classify(snapshot)
                        true
                    } catch (failure: Throwable) {
                        preserveInterruption(failure)
                        false
                    }
                })
            } ?: false
        } catch (failure: Throwable) {
            preserveInterruption(failure)
            false
        }
        return result?.takeIf { completed }
    }

    override fun bindTarget(ticketId: String, googleConversationId: String, snapshotToken: String): String =
        observation(scoped { it.bindNativeRcsTarget(ticketId, googleConversationId, snapshotToken) }, "target_bound")

    override fun bindFacts(ticketId: String, factsJson: String): Boolean = try {
        parse(scoped { it.bindNativeRcsFacts(ticketId, factsJson) }, SMALL_CAP, setOf("code"))["code"] == "facts_bound"
    } catch (failure: Throwable) {
        preserveInterruption(failure)
        false
    }

    override fun commit(ticketId: String, exactCommitJson: String): RcsNativeCommitResult = try {
        val response = LinkRuntime.withCapturedLinkOperation(lease) { it.commitNativeRcs(ticketId, exactCommitJson) }
        if (response == null) RcsNativeCommitResult.Rejected else when (parse(response, SMALL_CAP, setOf("code"))["code"]) {
            "accepted" -> RcsNativeCommitResult.Accepted
            "rejected" -> RcsNativeCommitResult.Rejected
            "members_changed" -> RcsNativeCommitResult.MembersChanged
            else -> RcsNativeCommitResult.OutcomeUnknown
        }
    } catch (failure: Throwable) {
        preserveInterruption(failure)
        RcsNativeCommitResult.OutcomeUnknown
    }

    override fun revoke(ticketId: String): RcsNativeRevokeResult = try {
        val response = lease.handle.revokeNativeSend(ticketId)
        when (parse(response, SMALL_CAP, setOf("code"))["code"]) {
            "revoked" -> RcsNativeRevokeResult.Revoked
            "late" -> RcsNativeRevokeResult.Late
            "none" -> RcsNativeRevokeResult.None
            else -> throw NativeRcsRevocationFailure()
        }
    } catch (failure: Throwable) {
        preserveInterruption(failure)
        throw NativeRcsRevocationFailure()
    }

    /** Go's only roster refusal at prepare is the exact one-field `{"code":"members_changed"}` reply. */
    private fun membersChangedOnly(response: String): Boolean =
        runCatching { parse(response, SMALL_CAP, setOf("code"))["code"] }.getOrNull() == "members_changed"

    private fun observation(response: String, code: String): String {
        val parsed = strictParse(response, OBSERVATION_CAP, setOf("code", "observation"))
        if (parsed["code"] != code) throw NativeRcsRefusal()
        return parsed.string("observation")?.takeIf(::withinObservationCap) ?: throw NativeRcsRefusal()
    }

    private fun scoped(operation: (LinkHandle) -> String): String =
        LinkRuntime.withCapturedLinkOperation(lease, operation) ?: throw NativeRcsRefusal()

    private fun strictParse(
        input: String,
        cap: Int,
        strings: Set<String>,
        numbers: Set<String> = emptySet(),
        ticketCapture: ((String) -> Unit)? = null,
    ): NativeResponse = try {
        parse(input, cap, strings, numbers, ticketCapture)
    } catch (failure: Throwable) {
        preserveInterruption(failure)
        throw NativeRcsRefusal()
    }

    private class NativeRcsRefusal : IllegalStateException("native_rcs_refused")
    private class NativeRcsRevocationFailure : IllegalStateException("native_rcs_revoke_failed")

    private companion object {
        const val SMALL_CAP = 4 * 1024
        const val OBSERVATION_CAP = 6 * 65_536 + 256
        const val PREPARE_CAP = 6 * 8 * 1024 * 1024 + 4096
    }
}

/** Shared wire builder for every Go observation that carries Android route evidence. */
internal fun RcsNativeRouteEvidence.carrierBindingJson(): JSONObject = JSONObject()
    .put("carrier_route_id", routeId)
    .put("android_subscription_id", subscriptionId)
    .put("own_number_raw", ownNumberRaw)
    .put("own_number_source", when (ownNumberSource) {
        RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber -> "subscription_manager_phone_number"
        RcsNativeOwnNumberSource.TelephonyLine1Number -> "telephony_line1_number"
    })
    .put("route_observed_at_ms", observedAtMs)

private fun preserveInterruption(failure: Throwable) {
    if (failure is InterruptedException) Thread.currentThread().interrupt()
}

private fun withinCandidateCap(value: String) = value.toByteArray(Charsets.UTF_8).size <= 8 * 1024 * 1024
private fun withinObservationCap(value: String) = value.toByteArray(Charsets.UTF_8).size <= 65_536

private fun ULong.toSignedLong(): Long? = takeIf { it in 1uL..Long.MAX_VALUE.toULong() }?.toLong()

private fun json(value: String): String = buildString(value.length + 2) {
    append('"')
    value.forEach { character -> when (character) {
        '\\' -> append("\\\\")
        '"' -> append("\\\"")
        '\b' -> append("\\b")
        '\u000C' -> append("\\f")
        '\n' -> append("\\n")
        '\r' -> append("\\r")
        '\t' -> append("\\t")
        else -> if (character < ' ') append("\\u%04x".format(character.code)) else append(character)
    } }
    append('"')
}

private class NativeResponse(private val fields: Map<String, String>) {
    operator fun get(name: String): String? = fields[name]
    fun string(name: String): String? = fields[name]
    fun uint64(name: String): ULong? = fields[name]?.takeIf { it.matches(Regex("0|[1-9][0-9]*")) }?.toULongOrNull()
}

private fun parse(
    input: String,
    cap: Int,
    stringFields: Set<String>,
    numberFields: Set<String> = emptySet(),
    ticketCapture: ((String) -> Unit)? = null,
): NativeResponse {
    if (input.toByteArray(Charsets.UTF_8).size > cap || !validEscapedUnicode(input)) throw IllegalStateException("native_response_invalid")
    val fields = stringFields + numberFields
    val values = HashMap<String, String>()
    JsonReader(StringReader(input)).use { reader ->
        reader.isLenient = false
        reader.beginObject()
        while (reader.hasNext()) {
            val name = reader.nextName()
            val expected = if (name in stringFields) JsonToken.STRING else JsonToken.NUMBER
            if (name !in fields || name in values || reader.peek() != expected) throw IllegalStateException("native_response_invalid")
            val value = reader.nextString()
            values[name] = value
            if (name == "ticket_id") ticketCapture?.invoke(value)
        }
        reader.endObject()
        if (reader.peek() != JsonToken.END_DOCUMENT || values.keys != fields) throw IllegalStateException("native_response_invalid")
    }
    return NativeResponse(values)
}

private fun validEscapedUnicode(input: String): Boolean {
    var raw = 0
    while (raw < input.length) {
        val character = input[raw++]
        if (character !in '\uD800'..'\uDFFF') continue
        if (character >= '\uDC00' || raw == input.length || input[raw++] !in '\uDC00'..'\uDFFF') return false
    }
    var index = 0
    while (index < input.length) {
        if (input[index++] != '\\') continue
        if (index == input.length) return false
        if (input[index++] != 'u') {
            if (input[index - 1] !in setOf('"', '\\', '/', 'b', 'f', 'n', 'r', 't')) return false
            continue
        }
        if (index + 4 > input.length) return false
        val first = unicodeEscapeValue(input, index) ?: return false
        index += 4
        if (first !in 0xD800..0xDFFF) continue
        if (first >= 0xDC00 || index + 6 > input.length || input[index] != '\\' || input[index + 1] != 'u') return false
        val second = unicodeEscapeValue(input, index + 2) ?: return false
        if (second !in 0xDC00..0xDFFF) return false
        index += 6
    }
    return true
}

private fun unicodeEscapeValue(input: String, start: Int): Int? {
    var value = 0
    for (index in start until start + 4) {
        value = value shl 4
        value += when (val character = input[index]) {
            in '0'..'9' -> character - '0'
            in 'a'..'f' -> character - 'a' + 10
            in 'A'..'F' -> character - 'A' + 10
            else -> return null
        }
    }
    return value
}

private fun validatedTicketId(value: String): String? {
    return runCatching { UUID.fromString(value).toString() }.getOrNull()?.takeIf { it == value.lowercase() }
}
