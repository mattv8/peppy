package dev.peppy.mobile.googlemessages.live

import android.util.JsonReader
import android.util.JsonToken
import java.io.StringReader
import java.util.ArrayDeque
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean

/** Disabled sideload-only contracts. No production code constructs this scaffold. */
internal interface LiveClock { fun elapsedRealtime(): Long }
internal interface LiveCancellation { val cancelled: Boolean }
internal class LiveCancelled : RuntimeException()
internal class LiveRejected : RuntimeException()

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
            socket?.let { closeQuietly(it, clock, cancellation) }
            throw failure
        }
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

    private fun closeQuietly(socket: LiveSocket, clock: LiveClock, cancellation: LiveCancellation) {
        try { close(socket, clock, cancellation) } catch (_: RuntimeException) { }
    }
    private fun checkLive(clock: LiveClock, deadline: Long, cancellation: LiveCancellation) {
        if (cancellation.cancelled) throw LiveCancelled()
        if (clock.elapsedRealtime() >= deadline) throw LiveRejected()
    }
    private fun validServerHello(raw: String) = SERVER_HELLO.matches(raw)
    internal fun bytes(value: String) = value.toByteArray(Charsets.UTF_8).size
    internal fun add(left: Long, right: Long): Long = try { Math.addExact(left, right) } catch (_: ArithmeticException) { throw LiveRejected() }
}

private val SERVER_HELLO = Regex("""\{\s*"type"\s*:\s*"hello"\s*,\s*"protocol_version"\s*:\s*2\s*,\s*"max_ttl_ms"\s*:\s*15000\s*,\s*"max_ciphertext_bytes"\s*:\s*5400\s*} """.trim())

internal enum class MembershipRole { OWNER, GATEWAY, DEVICE }
internal data class MembershipDevice(val deviceId: String, val revoked: Boolean, val keyEpoch: Long, val role: MembershipRole)
internal sealed interface MembershipControl {
    data class Bootstrap(val connectionId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Response(val connectionId: String, val challengeId: String, val membershipVersion: Long, val roster: List<MembershipDevice>) : MembershipControl
    data class Hint(val membershipVersion: Long) : MembershipControl
}

/** Bounded wire parser for disabled test seams; production must use the generated Rust contract bridge. */
internal object MembershipControls {
    fun parse(raw: String): MembershipControl? {
        if (LiveTransport.bytes(raw) > LiveTransport.MAX_CONTROL_BYTES) return null
        return try {
            JsonReader(StringReader(raw)).use { reader ->
                reader.isLenient = false
                val fields = objectFields(reader) ?: return null
                if (reader.peek() != JsonToken.END_DOCUMENT || fields.string("type") == null || fields.long("version") != 1L) return null
                when (fields.string("type")) {
                    "membership_hint" -> fields.exact("type", "version", "membership_version")
                        ?.let { fields.decimal("membership_version") }?.let(MembershipControl::Hint)
                    "membership_bootstrap" -> fields.exact("type", "version", "connection_id", "membership_version", "roster")
                        ?.let { bootstrap(fields) }
                    "membership_response" -> fields.exact("type", "version", "connection_id", "challenge_id", "membership_version", "roster")
                        ?.let { response(fields) }
                    else -> null
                }
            }
        } catch (_: Exception) { null }
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
private fun objectFields(reader: JsonReader): Fields? {
    if (reader.peek() != JsonToken.BEGIN_OBJECT) return null
    val values = linkedMapOf<String, Any>(); reader.beginObject()
    while (reader.hasNext()) {
        val name = reader.nextName(); if (name in values) return null
        values[name] = when (reader.peek()) {
            JsonToken.STRING -> reader.nextString(); JsonToken.NUMBER -> reader.nextLong(); JsonToken.BOOLEAN -> reader.nextBoolean()
            JsonToken.BEGIN_ARRAY -> { val items = mutableListOf<Fields>(); reader.beginArray(); while (reader.hasNext()) items += objectFields(reader) ?: return null; reader.endArray(); items }
            else -> return null
        }
    }
    reader.endObject(); return Fields(values)
}

internal data class LiveMembershipSnapshot(val connectionId: String, val version: Long, val activeRoster: List<MembershipDevice>)
internal interface LiveCore { fun enable(snapshot: LiveMembershipSnapshot); fun disable() }

/** One coordinator per captured native client. It never calls client code under a connection lock. */
internal class LiveIncarnationCoordinator {
    private val lock = Any()
    fun enable(publication: LivePublication) = synchronized(lock) {
        if (!publication.gate.get()) return@synchronized false
        publication.core.enable(publication.snapshot)
        if (!publication.gate.get()) publication.core.disable()
        publication.gate.get()
    }
    fun retire(publication: LivePublication) = synchronized(lock) {
        if (publication.enabled.compareAndSet(true, false)) publication.core.disable()
    }
}

internal class LivePublication internal constructor(internal val core: LiveCore, internal val snapshot: LiveMembershipSnapshot) {
    internal val gate = AtomicBoolean(true)
    internal val enabled = AtomicBoolean(false)
    fun close(coordinator: LiveIncarnationCoordinator) { gate.set(false); coordinator.retire(this) }
    fun closeAsync(coordinator: LiveIncarnationCoordinator) { gate.set(false); Thread { coordinator.retire(this) }.start() }
    fun admit(actionDeadline: Long): LiveAdmission = LiveAdmission(this, actionDeadline)
}
internal class LiveAdmission internal constructor(private val publication: LivePublication, private val actionDeadline: Long) {
    fun publish(clock: LiveClock, leaseDeadline: Long, action: () -> Unit): Boolean {
        val now = clock.elapsedRealtime()
        if (!publication.gate.get() || now >= leaseDeadline || now >= actionDeadline) return false
        action()
        return publication.gate.get() && clock.elapsedRealtime() < leaseDeadline
    }
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
    private var lastIssue = Long.MIN_VALUE; private var leaseDeadline = Long.MIN_VALUE; private var retired = false
    private var publication: LivePublication? = null; private var controlBytes = 0
    private val data = ArrayDeque<String>(64); private val controls = ArrayDeque<Pair<MembershipControl, Int>>(4)
    private data class Pending(val nonce: String, val issuedAt: Long, val responseDeadline: Long)

    fun bootstrap(control: MembershipControl.Bootstrap): Boolean = synchronized(state) {
        if (retired || connectionId != null || !MembershipControls.bootstrap(control.connectionId, control.membershipVersion, control.roster)) return@synchronized false
        connectionId = control.connectionId; highwater = control.membershipVersion; true
    }
    fun issue(nonce: String): Boolean = synchronized(state) {
        val now = clock.elapsedRealtime()
        if (!fresh(now) || pending != null || !validNonce(nonce)) return@synchronized false
        val deadline = try { LiveTransport.add(now, 5_000) } catch (_: LiveRejected) { retireLocked(); return@synchronized false }
        pending = Pending(nonce, now, deadline); lastIssue = now; true
    }
    /** Deterministic owner driver: callers supply CSPRNG bytes and dispatch the resulting control elsewhere. */
    fun drive(nextNonce: () -> String): String? = synchronized(state) {
        val now = clock.elapsedRealtime(); if (!fresh(now) || pending != null || (lastIssue != Long.MIN_VALUE && now - lastIssue < 20_000)) null
        else nextNonce().takeIf { issue(it) }
    }
    fun response(control: MembershipControl.Response): Boolean {
        val publicationToEnable: LivePublication
        synchronized(state) {
            val now = clock.elapsedRealtime(); val expected = pending ?: return false
            if (!fresh(now) || now > expected.responseDeadline || control.connectionId != connectionId || control.challengeId != expected.nonce ||
                !MembershipControls.response(control.connectionId, control.challengeId, control.membershipVersion, control.roster) || control.membershipVersion < highwater) {
                retireLocked(); return false
            }
            val deadline = try { LiveTransport.add(expected.issuedAt, 60_000) } catch (_: LiveRejected) { retireLocked(); return false }
            if (now >= deadline) { retireLocked(); return false }
            pending = null; highwater = control.membershipVersion; leaseDeadline = deadline
            if (publication != null && control.membershipVersion == publication!!.snapshot.version) return true
            publication?.close(coordinator)
            val roster = control.roster.filterNot { it.revoked }
            if (roster.none { it.deviceId == localDeviceId }) { retireLocked(); return false }
            publicationToEnable = LivePublication(core, LiveMembershipSnapshot(control.connectionId, control.membershipVersion, roster))
            publication = publicationToEnable
        }
        val enabled = coordinator.enable(publicationToEnable)
        synchronized(state) { if (!enabled || retired || publication !== publicationToEnable || !connectionGate.get()) { publicationToEnable.close(coordinator); if (publication === publicationToEnable) publication = null; return false }; publicationToEnable.enabled.set(true); return true }
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
    internal fun enabled() = synchronized(state) { publication?.enabled?.get() == true }
    internal fun highwater() = synchronized(state) { highwater }
    private fun fresh(now: Long) = !retired && connectionGate.get() && connectionId != null && (publication == null || now < leaseDeadline)
    private fun retireLocked() { if (retired) return; retired = true; connectionGate.set(false); pending = null; data.clear(); controls.clear(); controlBytes = 0; publication?.closeAsync(coordinator); publication = null }
    private fun validNonce(value: String) = try { value.length == 22 && Base64.getUrlDecoder().decode(value).let { it.size == 16 && Base64.getUrlEncoder().withoutPadding().encodeToString(it) == value } } catch (_: IllegalArgumentException) { false }
}
