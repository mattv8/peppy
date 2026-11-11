package dev.peppy.mobile.googlemessages.live

import java.util.ArrayDeque
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/** Membership transport contracts shared by the production live host and its tests. */
internal interface LiveClock { fun elapsedRealtime(): Long }
internal interface LiveCancellation { val cancelled: Boolean }
internal class LiveCancelled : RuntimeException()
internal class LiveRejected : RuntimeException()

internal sealed interface LiveSocketFrame {
    data class Text(val value: String) : LiveSocketFrame
    data object Binary : LiveSocketFrame
    data object Closed : LiveSocketFrame
}

internal sealed interface LiveInboundFrame {
    data class Data(val value: String) : LiveInboundFrame
    data class Control(val control: MembershipControl, val raw: String) : LiveInboundFrame
}

internal interface LiveSocket {
    fun receive(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocketFrame
    fun send(text: String, deadlineElapsedMs: Long, cancellation: LiveCancellation)
    fun close(deadlineElapsedMs: Long, cancellation: LiveCancellation)
}

/** The opaque lease alone constructs the fixed canonical-origin bearer request. */
internal interface LiveLease { fun open(deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveSocket }
internal interface LiveLeaseFactory { fun acquire(): LiveLease? }

internal object LiveTransport {
    const val MAX_TEXT_BYTES = 8_192
    const val MAX_CONTROL_BYTES = 32 * 1024
    const val HANDSHAKE_MS = 5_000L
    const val CLOSE_MS = 1_000L

    fun open(lease: LiveLease, clock: LiveClock, cancellation: LiveCancellation): LiveSocket {
        val deadline = add(clock.elapsedRealtime(), HANDSHAKE_MS)
        checkLive(clock, deadline, cancellation)
        var socket: LiveSocket? = null
        try {
            socket = lease.open(deadline, cancellation)
            checkLive(clock, deadline, cancellation)
            val hello = socket.receive(deadline, cancellation) as? LiveSocketFrame.Text ?: throw LiveRejected()
            checkLive(clock, deadline, cancellation)
            if (bytes(hello.value) > MAX_TEXT_BYTES || !validServerHello(hello.value)) throw LiveRejected()
            return socket
        } catch (failure: RuntimeException) {
            socket?.let { closeQuietly(it, clock) }
            throw failure
        }
    }

    fun receiveText(socket: LiveSocket, clock: LiveClock, deadlineElapsedMs: Long, cancellation: LiveCancellation): LiveInboundFrame {
        checkLive(clock, deadlineElapsedMs, cancellation)
        val frame = socket.receive(deadlineElapsedMs, cancellation)
        checkLive(clock, deadlineElapsedMs, cancellation)
        val text = (frame as? LiveSocketFrame.Text)?.value ?: throw LiveRejected()
        val byteCount = bytes(text)
        if (byteCount > MAX_CONTROL_BYTES) throw LiveRejected()
        MembershipControls.parse(text)?.let { return LiveInboundFrame.Control(it, text) }
        if (byteCount > MAX_TEXT_BYTES) throw LiveRejected()
        return LiveInboundFrame.Data(text)
    }

    fun write(socket: LiveSocket, value: String, clock: LiveClock, deadlineElapsedMs: Long, cancellation: LiveCancellation) {
        if (bytes(value) > MAX_TEXT_BYTES) throw LiveRejected()
        checkLive(clock, deadlineElapsedMs, cancellation)
        socket.send(value, deadlineElapsedMs, cancellation)
        checkLive(clock, deadlineElapsedMs, cancellation)
    }

    fun close(socket: LiveSocket, clock: LiveClock, cancellation: LiveCancellation) {
        val deadline = add(clock.elapsedRealtime(), CLOSE_MS)
        socket.close(deadline, cancellation)
    }

    private fun closeQuietly(socket: LiveSocket, clock: LiveClock) {
        try { close(socket, clock, NeverCancelled) } catch (_: RuntimeException) { }
    }

    private data object NeverCancelled : LiveCancellation {
        override val cancelled = false
    }
    private fun checkLive(clock: LiveClock, deadline: Long, cancellation: LiveCancellation) {
        if (cancellation.cancelled) throw LiveCancelled()
        if (clock.elapsedRealtime() >= deadline) throw LiveRejected()
    }
    private fun validServerHello(raw: String) = SERVER_HELLO.matches(raw)
    internal fun bytes(value: String) = value.toByteArray(Charsets.UTF_8).size
    internal fun add(left: Long, right: Long): Long = try { Math.addExact(left, right) } catch (_: ArithmeticException) { throw LiveRejected() }
}

private val SERVER_HELLO = Regex("""\{\s*"type"\s*:\s*"hello"\s*,\s*"protocol_version"\s*:\s*2\s*,\s*"max_ttl_ms"\s*:\s*15000\s*,\s*"max_ciphertext_bytes"\s*:\s*5400\s*\} """.trim())

internal enum class MembershipRole { OWNER, GATEWAY, DEVICE }
internal data class MembershipDevice(val deviceId: String, val revoked: Boolean, val keyEpoch: Long, val role: MembershipRole)
internal sealed interface MembershipControl {
    data class Bootstrap(val connectionId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Response(val connectionId: String, val challengeId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Hint(val membershipVersion: Long) : MembershipControl
}

/** Bounded parser for membership controls accepted from the live server. */
internal object MembershipControls {
    fun parse(raw: String): MembershipControl? {
        if (LiveTransport.bytes(raw) > LiveTransport.MAX_CONTROL_BYTES) return null
        val fields = StrictJsonParser(raw).parseObject() ?: return null
        if (fields.string("type") == null || fields.long("version") != 1L) return null
        return when (fields.string("type")) {
            "membership_hint" -> fields.exact("type", "version", "membership_version")
                ?.let { fields.decimal("membership_version") }?.let(MembershipControl::Hint)
            "membership_bootstrap" -> fields.exact("type", "version", "connection_id", "membership_version", "roster")
                ?.let { bootstrap(fields) }
            "membership_response" -> fields.exact("type", "version", "connection_id", "challenge_id", "membership_version", "roster")
                ?.let { response(fields) }
            else -> null
        }
    }

    fun bootstrap(connectionId: String, version: Long, roster: List<MembershipDevice>) = validId(connectionId) && validVersion(version) && validRoster(roster)
    fun response(connectionId: String, challengeId: String, version: Long, roster: List<MembershipDevice>) = bootstrap(connectionId, version, roster) && validId(challengeId)
    fun validVersion(value: Long) = value > 0

    private fun bootstrap(fields: Fields): MembershipControl.Bootstrap? {
        val control = MembershipControl.Bootstrap(fields.string("connection_id") ?: return null, fields.decimal("membership_version") ?: return null, fields.roster("roster") ?: return null)
        return control.takeIf { bootstrap(it.connectionId, it.membershipVersion, it.roster) }
    }
    private fun response(fields: Fields): MembershipControl.Response? {
        val control = MembershipControl.Response(fields.string("connection_id") ?: return null, fields.string("challenge_id") ?: return null, fields.decimal("membership_version") ?: return null, fields.roster("roster") ?: return null)
        return control.takeIf { response(it.connectionId, it.challengeId, it.membershipVersion, it.roster) }
    }
    private fun validId(value: String): Boolean = try {
        value.length == 22 && Base64.getUrlDecoder().decode(value).let { it.size == 16 && Base64.getUrlEncoder().withoutPadding().encodeToString(it) == value }
    } catch (_: IllegalArgumentException) { false }
    private fun validRoster(roster: List<MembershipDevice>) = roster.isNotEmpty() && roster.size <= 100 && roster.map { it.deviceId }.toSet().size == roster.size && roster.all {
        it.deviceId.matches(UUID) && it.keyEpoch in 1..0xffff_ffffL
    }
}

private val UUID = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
private class Fields(private val values: Map<String, Any>) {
    fun exact(vararg names: String) = takeIf { values.keys == names.toSet() }
    fun string(name: String) = values[name] as? String
    fun long(name: String) = values[name] as? Long
    fun decimal(name: String) = string(name)?.takeIf { it.matches(Regex("[1-9][0-9]{0,18}")) }?.toLongOrNull()?.takeIf { it > 0 }
    fun roster(name: String): List<MembershipDevice>? = (values[name] as? List<*>)?.map {
        val device = it as? Fields ?: return null
        if (device.exact("device_id", "revoked", "key_epoch", "role") == null) return null
        MembershipDevice(device.string("device_id") ?: return null, device.values["revoked"] as? Boolean ?: return null,
            device.long("key_epoch") ?: return null, when (device.string("role")) { "owner" -> MembershipRole.OWNER; "gateway" -> MembershipRole.GATEWAY; "device" -> MembershipRole.DEVICE; else -> return null })
    }
}
private class StrictJsonParser(private val input: String) {
    private var offset = 0

    fun parseObject(): Fields? {
        val value = objectValue(0) ?: return null
        whitespace()
        return value.takeIf { offset == input.length }
    }

    private fun objectValue(depth: Int): Fields? {
        if (depth > MAX_JSON_DEPTH || !consume('{')) return null
        val values = linkedMapOf<String, Any>()
        whitespace()
        if (consume('}')) return Fields(values)
        while (true) {
            val name = stringValue() ?: return null
            if (name in values || !consume(':')) return null
            values[name] = value(depth + 1) ?: return null
            when {
                consume('}') -> return Fields(values)
                consume(',') -> Unit
                else -> return null
            }
        }
    }

    private fun value(depth: Int): Any? {
        whitespace()
        return when (input.getOrNull(offset)) {
            '"' -> stringValue()
            '{' -> objectValue(depth)
            '[' -> arrayValue(depth)
            't' -> literal("true", true)
            'f' -> literal("false", false)
            in '0'..'9' -> numberValue()
            else -> null
        }
    }

    private fun arrayValue(depth: Int): List<Fields>? {
        if (!consume('[')) return null
        val values = mutableListOf<Fields>()
        whitespace()
        if (consume(']')) return values
        while (true) {
            values += objectValue(depth + 1) ?: return null
            when {
                consume(']') -> return values
                consume(',') -> Unit
                else -> return null
            }
        }
    }

    private fun stringValue(): String? {
        whitespace()
        if (input.getOrNull(offset) != '"') return null
        offset++
        val result = StringBuilder()
        while (offset < input.length) {
            val character = input[offset++]
            when {
                character == '"' -> return result.toString()
                character == '\\' -> result.append(escapedCharacter() ?: return null)
                character < ' ' -> return null
                else -> result.append(character)
            }
        }
        return null
    }

    private fun escapedCharacter(): String? = when (val escaped = input.getOrNull(offset++)) {
        '"', '\\', '/' -> escaped.toString()
        'b' -> "\b"
        'f' -> "\u000c"
        'n' -> "\n"
        'r' -> "\r"
        't' -> "\t"
        'u' -> unicodeCharacter()
        else -> null
    }

    private fun unicodeCharacter(): String? {
        val first = unicodeCodeUnit() ?: return null
        if (!first.isSurrogate()) return first.toString()
        if (!first.isHighSurrogate() || input.getOrNull(offset) != '\\' || input.getOrNull(offset + 1) != 'u') return null
        offset += 2
        val second = unicodeCodeUnit() ?: return null
        if (!second.isLowSurrogate()) return null
        return charArrayOf(first, second).concatToString()
    }

    private fun unicodeCodeUnit(): Char? {
        if (offset + 4 > input.length) return null
        val digits = input.subSequence(offset, offset + 4)
        if (!digits.all(::isAsciiHexDigit)) return null
        offset += 4
        return digits.toString().toInt(16).toChar()
    }

    private fun isAsciiHexDigit(character: Char) =
        character in '0'..'9' || character in 'a'..'f' || character in 'A'..'F'

    private fun numberValue(): Long? {
        whitespace()
        val start = offset
        if (input.getOrNull(offset) == '0') {
            offset++
            if (input.getOrNull(offset) in '0'..'9') return null
        } else {
            while (input.getOrNull(offset) in '0'..'9') offset++
        }
        return input.substring(start, offset).toLongOrNull()
    }

    private fun literal(expected: String, value: Boolean): Boolean? {
        if (!input.startsWith(expected, offset)) return null
        offset += expected.length
        return value
    }

    private fun consume(expected: Char): Boolean {
        whitespace()
        if (input.getOrNull(offset) != expected) return false
        offset++
        return true
    }

    private fun whitespace() {
        while (input.getOrNull(offset)?.let(::isJsonWhitespace) == true) offset++
    }

    private fun isJsonWhitespace(character: Char) =
        character == ' ' || character == '\n' || character == '\r' || character == '\t'

    private companion object {
        const val MAX_JSON_DEPTH = 3
    }
}

internal data class LiveMembershipSnapshot(val connectionId: String, val version: Long, val activeRoster: List<MembershipDevice>)
internal interface LiveCore { fun enable(snapshot: LiveMembershipSnapshot); fun disable() }

/** One coordinator per captured native client. It never calls client code under a connection lock. */
internal class LiveIncarnationCoordinator {
    private val lock = Any()
    private var active: LivePublication? = null

    fun enable(publication: LivePublication) = synchronized(lock) {
        if (!publication.isOpen()) return@synchronized false
        active?.takeIf { it !== publication }?.let { previous ->
            previous.closeGate()
            disable(previous)
        }
        publication.core.enable(publication.snapshot)
        publication.enabled.set(true)
        if (!publication.isOpen()) {
            disable(publication)
            return@synchronized false
        }
        active = publication
        true
    }

    fun retire(publication: LivePublication) = synchronized(lock) {
        disable(publication)
        if (active === publication) active = null
    }

    private fun disable(publication: LivePublication) {
        if (publication.enabled.compareAndSet(true, false)) publication.core.disable()
    }
}

internal class LivePublication internal constructor(
    internal val core: LiveCore,
    internal val snapshot: LiveMembershipSnapshot,
    private val clock: LiveClock,
    leaseDeadline: Long,
) {
    internal val gate = AtomicBoolean(true)
    internal val enabled = AtomicBoolean(false)
    private val membershipLeaseDeadline = AtomicLong(leaseDeadline)
    private val operationDeadline = AtomicLong(Long.MAX_VALUE)

    fun close(coordinator: LiveIncarnationCoordinator) {
        closeGate()
        coordinator.retire(this)
    }

    fun closeAsync(coordinator: LiveIncarnationCoordinator) {
        closeGate()
        Thread { coordinator.retire(this) }.start()
    }

    internal fun closeGate() { gate.set(false) }
    internal fun isOpen() = gate.get()
    internal fun renew(deadline: Long) { membershipLeaseDeadline.set(deadline); operationDeadline.set(Long.MAX_VALUE) }
    internal fun beginOperation(deadline: Long) { operationDeadline.set(deadline) }
    fun admit(actionDeadline: Long): LiveAdmission = LiveAdmission(this, actionDeadline)

    internal fun publish(actionDeadline: Long, afterRuntimeWait: () -> Unit, effect: () -> Unit): Boolean {
        afterRuntimeWait()
        if (!enabled.get() || !gate.compareAndSet(true, true)) return false
        val admittedAt = clock.elapsedRealtime()
        if (admittedAt >= membershipLeaseDeadline.get() || admittedAt > operationDeadline.get() || admittedAt >= actionDeadline) return false
        effect()
        return true
    }
}
internal class LiveAdmission internal constructor(private val publication: LivePublication, private val actionDeadline: Long) {
    fun publish(afterRuntimeWait: () -> Unit, effect: () -> Unit) = publication.publish(actionDeadline, afterRuntimeWait, effect)
}

/**
 * Lock order: connection state lock is never held while coordinator/core/socket work runs;
 * coordinator lock only calls core. The atomic publication gate is checked at every publication.
 */
internal class LiveConnection(
    private val clock: LiveClock,
    private val core: LiveCore,
    private val localDeviceId: String,
    private val coordinator: LiveIncarnationCoordinator = LiveIncarnationCoordinator(),
) {
    private val state = Any(); private val connectionGate = AtomicBoolean(true)
    private var connectionId: String? = null; private var highwater = 0L; private var pending: Pending? = null
    private var lastIssue = Long.MIN_VALUE; private var lastNonce: String? = null
    private var leaseDeadline = Long.MIN_VALUE; private var retired = false; private var selfRevoked = false
    private var publication: LivePublication? = null; private var controlBytes = 0
    private val data = ArrayDeque<String>(64); private val controls = ArrayDeque<Pair<MembershipControl, Int>>(4)
    private data class Pending(val nonce: String, val issuedAt: Long, val responseDeadline: Long)

    fun bootstrap(control: MembershipControl.Bootstrap): Boolean = synchronized(state) {
        if (retired || connectionId != null || !MembershipControls.bootstrap(control.connectionId, control.membershipVersion, control.roster)) return@synchronized false
        connectionId = control.connectionId; highwater = control.membershipVersion; true
    }
    fun issue(nonce: String): Boolean = synchronized(state) { issueLocked(nonce, clock.elapsedRealtime()) }

    /** Deterministic owner driver: callers supply CSPRNG bytes and a bounded control enqueue. */
    fun drive(nextNonce: () -> String, enqueue: (String) -> Boolean): String? {
        val nonce = synchronized(state) {
            val now = clock.elapsedRealtime()
            if (!fresh(now) || pending != null || !cadenceElapsed(now)) return null
            nextNonce().takeIf { issueLocked(it, now) } ?: return null
        }
        val queued = try { enqueue(nonce) } catch (_: RuntimeException) { false }
        if (!queued) {
            retire()
            return null
        }
        return synchronized(state) { nonce.takeIf { !retired && pending?.nonce == nonce } }
    }
    fun response(control: MembershipControl.Response): Boolean {
        var publicationToRetire: LivePublication? = null
        var publicationToEnable: LivePublication? = null
        synchronized(state) {
            val now = clock.elapsedRealtime(); val expected = pending ?: return false
            if (!fresh(now) || now > expected.responseDeadline || control.connectionId != connectionId || control.challengeId != expected.nonce ||
                !MembershipControls.response(control.connectionId, control.challengeId, control.membershipVersion, control.roster) || control.membershipVersion < highwater) {
                retireLocked(); return false
            }
            val deadline = try { LiveTransport.add(expected.issuedAt, 60_000) } catch (_: LiveRejected) { retireLocked(); return false }
            if (now >= deadline) { retireLocked(); return false }
            val roster = control.roster.filterNot { it.revoked }
            if (roster.none { it.deviceId == localDeviceId }) { selfRevoked = true; retireLocked(); return false }
            pending = null
            highwater = control.membershipVersion
            val current = publication
            if (current != null && control.membershipVersion == current.snapshot.version) {
                leaseDeadline = deadline
                current.renew(deadline)
                return true
            }
            if (current != null) {
                publication = null
                leaseDeadline = Long.MIN_VALUE
                publicationToRetire = current
            } else {
                leaseDeadline = deadline
                publicationToEnable = LivePublication(core, LiveMembershipSnapshot(control.connectionId, control.membershipVersion, roster), clock, deadline)
                publication = publicationToEnable
            }
        }
        publicationToRetire?.let {
            it.close(coordinator)
            return true
        }
        val target = publicationToEnable ?: return false
        val enabled = coordinator.enable(target)
        val accepted = synchronized(state) {
            val current = enabled && !retired && publication === target && connectionGate.get()
            if (!current && publication === target) publication = null
            current
        }
        if (!accepted) target.close(coordinator)
        return accepted
    }
    fun hint(control: MembershipControl.Hint): Boolean {
        val toClose = synchronized(state) {
            if (retired || !MembershipControls.validVersion(control.membershipVersion)) return false
            if (control.membershipVersion <= highwater) return true
            highwater = control.membershipVersion; pending = null; publication.also { publication = null }
        }
        toClose?.close(coordinator); return true
    }
    fun tick(): Boolean = synchronized(state) { val now = clock.elapsedRealtime(); if (!fresh(now) || pending?.let { now > it.responseDeadline } == true || (publication != null && now >= leaseDeadline)) { retireLocked(); false } else true }
    fun offerData(frame: String): Boolean = synchronized(state) { if (retired || LiveTransport.bytes(frame) > LiveTransport.MAX_TEXT_BYTES || data.size == 64) false else { data.addLast(frame); true } }
    fun offerControl(control: MembershipControl, raw: String): Boolean = synchronized(state) {
        val bytes = LiveTransport.bytes(raw)
        if (retired) return false
        if (bytes > LiveTransport.MAX_CONTROL_BYTES || controls.size == 4 || controlBytes > LiveTransport.MAX_CONTROL_BYTES - bytes) { retireLocked(); return false }
        controls.addLast(control to bytes); controlBytes += bytes; true
    }
    fun admit(actionDeadline: Long): LiveAdmission? = synchronized(state) { if (!tick() || !connectionGate.get()) null else publication?.admit(actionDeadline) }
    fun retire() = synchronized(state) { retireLocked() }
    fun selfRevoked() = synchronized(state) { selfRevoked }
    internal fun enabled() = synchronized(state) { publication?.enabled?.get() == true }
    internal fun highwater() = synchronized(state) { highwater }
    private fun fresh(now: Long) = !retired && connectionGate.get() && connectionId != null && (publication == null || now < leaseDeadline)
    private fun cadenceElapsed(now: Long) = lastIssue == Long.MIN_VALUE || (now >= lastIssue && now - lastIssue >= 20_000)
    private fun issueLocked(nonce: String, now: Long): Boolean {
        if (!fresh(now) || pending != null || !cadenceElapsed(now) || nonce == lastNonce || !validNonce(nonce)) return false
        val deadline = try { LiveTransport.add(now, 5_000) } catch (_: LiveRejected) { retireLocked(); return false }
        pending = Pending(nonce, now, deadline)
        lastIssue = now
        lastNonce = nonce
        publication?.beginOperation(deadline)
        return true
    }
    private fun retireLocked() { if (retired) return; retired = true; connectionGate.set(false); pending = null; data.clear(); controls.clear(); controlBytes = 0; publication?.closeAsync(coordinator); publication = null }
    private fun validNonce(value: String) = try { value.length == 22 && Base64.getUrlDecoder().decode(value).let { it.size == 16 && Base64.getUrlEncoder().withoutPadding().encodeToString(it) == value } } catch (_: IllegalArgumentException) { false }
}
