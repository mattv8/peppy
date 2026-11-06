package dev.peppy.mobile.googlemessages.live

import android.util.JsonReader
import android.util.JsonToken
import java.io.StringReader
import java.util.ArrayDeque
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean

/** Disabled sideload-only contracts. Nothing in production constructs this scaffold yet. */
internal interface LiveClock { fun elapsedRealtime(): Long }
internal interface LiveCancellation { val cancelled: Boolean }
internal class LiveCancelled : RuntimeException()
internal class LiveRejected : RuntimeException()

internal class LiveRequest internal constructor(val path: String, internal val bearer: String) {
    init { require(path == "/v1/live") }
}

internal sealed interface LiveSocketFrame {
    data class Text(val value: String) : LiveSocketFrame
    data object Binary : LiveSocketFrame
    data object Closed : LiveSocketFrame
}

internal interface LiveSocket {
    fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocketFrame
    fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation)
    fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation)
}

/** A lease is the only capability that can form the fixed bearer request. */
internal interface LiveLease { fun open(request: LiveRequest, deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocket }
internal interface LiveLeaseFactory { fun acquire(): LiveLease? }

internal object LiveTransport {
    const val MAX_TEXT_BYTES = 8_192
    const val MAX_CONTROL_BYTES = 32 * 1024
    const val HANDSHAKE_MS = 5_000L
    const val CLOSE_MS = 1_000L

    fun open(lease: LiveLease, bearer: String, clock: LiveClock, cancellation: LiveCancellation): LiveSocket {
        val deadline = add(clock.elapsedRealtime(), HANDSHAKE_MS)
        checkCancelled(cancellation)
        val socket = lease.open(LiveRequest("/v1/live", bearer), deadline, cancellation)
        val hello = socket.receive(deadline, cancellation) as? LiveSocketFrame.Text ?: throw LiveRejected()
        if (bytes(hello.value) > MAX_TEXT_BYTES || !validServerHello(hello.value)) throw LiveRejected()
        return socket
    }

    fun write(socket: LiveSocket, value: String, deadlineElapsedMs: Long, cancellation: LiveCancellation) {
        if (bytes(value) > MAX_TEXT_BYTES) throw LiveRejected()
        checkCancelled(cancellation)
        socket.send(value, deadlineElapsedMs, cancellation)
        checkCancelled(cancellation)
    }

    fun close(socket: LiveSocket, clock: LiveClock, cancellation: LiveCancellation) =
        socket.close(add(clock.elapsedRealtime(), CLOSE_MS), cancellation)

    private fun validServerHello(raw: String): Boolean = SERVER_HELLO.matches(raw)

    internal fun bytes(value: String) = value.toByteArray(Charsets.UTF_8).size
    internal fun add(left: Long, right: Long): Long = try { Math.addExact(left, right) } catch (_: ArithmeticException) { throw LiveRejected() }
    internal fun checkCancelled(cancellation: LiveCancellation) { if (cancellation.cancelled) throw LiveCancelled() }
}

private val SERVER_HELLO = Regex("""\{\s*"type"\s*:\s*"hello"\s*,\s*"protocol_version"\s*:\s*2\s*,\s*"max_ttl_ms"\s*:\s*15000\s*,\s*"max_ciphertext_bytes"\s*:\s*5400\s*} """.trim())

internal enum class MembershipRole { OWNER, GATEWAY, DEVICE }
internal data class MembershipDevice(val deviceId: String, val revoked: Boolean, val keyEpoch: Int, val role: MembershipRole)
internal sealed interface MembershipControl {
    data class Bootstrap(val connectionId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Response(val connectionId: String, val challengeId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Hint(val membershipVersion: Long) : MembershipControl
}

/** Strict bounded Kotlin mirror for the disabled Rust-owned membership DTOs. */
internal object MembershipControls {
    fun parse(raw: String): MembershipControl? {
        if (LiveTransport.bytes(raw) > LiveTransport.MAX_CONTROL_BYTES) return null
        return parseValues(raw)?.let { values ->
            if (values["version"] != "1") return@let null
            when (values["type"]) {
                "membership_hint" -> values.takeIf { it.keys == setOf("type", "version", "membership_version") }
                    ?.get("membership_version")?.let(::version)?.let(MembershipControl::Hint)
                else -> null // The production Rust parser owns bootstrap/response roster decoding.
            }
        }
    }

    fun bootstrap(connectionId: String, version: Long, roster: List<MembershipDevice>) =
        validConnection(connectionId) && validVersion(version) && validRoster(roster)
    fun response(connectionId: String, challengeId: String, version: Long, roster: List<MembershipDevice>) =
        bootstrap(connectionId, version, roster) && validConnection(challengeId)
    fun validVersion(value: Long) = value in 1..Long.MAX_VALUE
    private fun version(raw: String) = raw.takeIf { it.matches(Regex("[1-9][0-9]{0,18}")) }?.toLongOrNull()?.takeIf(::validVersion)
    private fun validConnection(value: String): Boolean = try {
        value.length == 22 && Base64.getUrlDecoder().decode(value).let { it.size == 16 && Base64.getUrlEncoder().withoutPadding().encodeToString(it) == value }
    } catch (_: IllegalArgumentException) { false }
    private fun validRoster(roster: List<MembershipDevice>) = roster.size <= 100 && roster.map { it.deviceId }.toSet().size == roster.size && roster.all {
        it.deviceId.matches(Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")) && it.keyEpoch > 0
    }
}

private fun parseValues(raw: String): Map<String, String>? {
    val values = linkedMapOf<String, String>()
    return try {
        JsonReader(StringReader(raw)).use { reader ->
            var valid = true
            reader.isLenient = false; reader.beginObject()
            while (reader.hasNext()) {
                val name = reader.nextName()
                if (name in values || reader.peek() !in setOf(JsonToken.STRING, JsonToken.NUMBER, JsonToken.BOOLEAN)) {
                    valid = false
                    reader.skipValue()
                } else values[name] = reader.nextString()
            }
            reader.endObject()
            if (valid && reader.peek() == JsonToken.END_DOCUMENT) values else null
        }
    } catch (_: Exception) { null }
}

internal interface LiveCore { fun enable(); fun disable() }
internal class LivePublication(private val core: LiveCore) {
    private val enabled = AtomicBoolean(false)
    fun enable() { if (enabled.compareAndSet(false, true)) core.enable() }
    fun retire() { if (enabled.compareAndSet(true, false)) core.disable() }
    fun publish(now: Long, leaseDeadline: Long, actionDeadline: Long, action: () -> Unit): Boolean {
        if (!enabled.get() || now >= leaseDeadline || now >= actionDeadline || !enabled.get()) return false
        action()
        return enabled.get()
    }
}

/** Connection-owned control state. Core publication can be retired independently. */
internal class LiveConnection(private val clock: LiveClock, private val core: LiveCore) {
    private var connectionId: String? = null
    private var highwater = 0L
    private var pending: Pending? = null
    private var lastIssue = Long.MIN_VALUE
    private var leaseDeadline = Long.MIN_VALUE
    private var retired = false
    private var publication: LivePublication? = null
    private val data = ArrayDeque<String>(64)
    private val controls = ArrayDeque<MembershipControl>(4)
    private data class Pending(val nonce: String, val issuedAt: Long, val responseDeadline: Long)

    fun bootstrap(control: MembershipControl.Bootstrap): Boolean = synchronized(this) {
        if (retired || !MembershipControls.bootstrap(control.connectionId, control.membershipVersion, control.roster)) return false
        connectionId = control.connectionId; highwater = maxOf(highwater, control.membershipVersion); pending = null; true
    }
    fun issue(nonce: String): Boolean = synchronized(this) {
        val now = clock.elapsedRealtime()
        if (retired || connectionId == null || pending != null || (lastIssue != Long.MIN_VALUE && now - lastIssue < 20_000L) || !validNonce(nonce)) return false
        pending = Pending(nonce, now, LiveTransport.add(now, 5_000L)); lastIssue = now; true
    }
    fun response(control: MembershipControl.Response): Boolean = synchronized(this) {
        val now = clock.elapsedRealtime(); val expected = pending ?: return false
        if (retired || now > expected.responseDeadline || control.connectionId != connectionId || control.challengeId != expected.nonce ||
            !MembershipControls.response(control.connectionId, control.challengeId, control.membershipVersion, control.roster) || control.membershipVersion < highwater) return false
        pending = null
        if (control.membershipVersion > highwater && publication != null) { publication?.retire(); publication = null; highwater = control.membershipVersion; return false }
        highwater = maxOf(highwater, control.membershipVersion)
        leaseDeadline = LiveTransport.add(expected.issuedAt, 60_000L)
        if (now >= leaseDeadline) { retire(); return false }
        if (publication == null) publication = LivePublication(core).also { it.enable() }
        true
    }
    fun hint(control: MembershipControl.Hint): Boolean = synchronized(this) {
        if (!MembershipControls.validVersion(control.membershipVersion)) return false
        if (control.membershipVersion > highwater) { highwater = control.membershipVersion; publication?.retire(); publication = null }; true
    }
    fun tick() = synchronized(this) { if (retired) false else if (pending?.let { clock.elapsedRealtime() > it.responseDeadline } == true || (publication != null && clock.elapsedRealtime() >= leaseDeadline)) { retire(); false } else true }
    fun offerData(frame: String): Boolean = synchronized(this) { if (LiveTransport.bytes(frame) > LiveTransport.MAX_TEXT_BYTES || data.size == 64) false else { data.addLast(frame); true } }
    fun offerControl(control: MembershipControl, rawBytes: Int): Boolean = synchronized(this) { if (rawBytes > LiveTransport.MAX_CONTROL_BYTES || controls.size == 4) { retire(); false } else { controls.addLast(control); true } }
    fun publish(actionDeadline: Long, action: () -> Unit) = synchronized(this) { publication?.publish(clock.elapsedRealtime(), leaseDeadline, actionDeadline, action) ?: false }
    fun retire() { publication?.retire(); publication = null; pending = null; retired = true; data.clear(); controls.clear() }
    internal fun enabled() = publication != null
    internal fun highwater() = highwater
    private fun validNonce(value: String) = value.length == 22 && runCatching { Base64.getUrlDecoder().decode(value) }.getOrNull()?.size == 16
}
