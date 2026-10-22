package dev.peppy.mobile

import android.os.Debug
import android.os.SystemClock
import android.util.Log
import androidx.annotation.VisibleForTesting
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicLong

internal enum class SyncTraceEvent(val code: Long) {
    OBSERVER_HINT(1), OBSERVER_DISPATCH(2), CAPTURE_ENQUEUE(3), CAPTURE_START(4), CAPTURE_END(5), CAPTURE_RETRY(6), CAPTURE_CONTINUATION(7), CAPTURE_PLAN(8), CAPTURE_SUMMARY(9), PROVIDER_QUERY(10), NATIVE_OPEN(11), GATEWAY_ENQUEUE(12), GATEWAY_START(13), GATEWAY_END(14), GATEWAY_PHASE(15), HTTP_REQUEST(16), OUTBOX_ACK(17),
}

internal enum class SyncTraceField(val code: Long) {
    TRACE_ID(1), PARENT_ID(2), ENQUEUED_MS(3), DELAY_MS(4), ATTEMPT(5), DURATION_MS(6), CPU_MS(7), SUCCESS(8), SOURCE(9), CAPTURED(10), PENDING(11), UNAVAILABLE(12), PAUSED(13), MORE(14), PROVIDER_ERROR(15), CORE_ERROR(16), IMPORTED(17), ROWS(18), COUNT(19), PHASE(20), METHOD(21), ROUTE(22), STATUS(23), REQUEST_BYTES(24), RESPONSE_BYTES(25), COLD(26), OUTCOME(28),
}

/** Debug-only, fixed-vocabulary timing telemetry. Values are numeric to prevent content logging. */
internal object SyncDiagnostics {
    private data class Configuration(
        val enabled: Boolean,
        val clock: () -> Long,
        val cpuMs: () -> Long,
        val sink: (String) -> Unit,
        val sessionToken: Long?,
    )

    private val trace = ThreadLocal<Long>()
    private val sequence = AtomicLong(1)
    @Volatile private var session = 0L
    @Volatile private var configuration = Configuration(
        enabled = BuildConfig.PEPPY_SMS_DIAGNOSTICS,
        clock = SystemClock::elapsedRealtime,
        cpuMs = { Debug.threadCpuTimeNanos() / NANOS_PER_MILLISECOND },
        sink = { Log.d(TAG, it) },
        sessionToken = null,
    )

    val enabled: Boolean get() = configuration.enabled

    fun nowMs(): Long = if (enabled) configuration.clock() else 0L

    fun newTraceId(): Long = if (enabled) sequence.getAndIncrement() else 0L

    fun currentTraceId(): Long = if (enabled) trace.get() ?: 0L else 0L

    /** Process-local token; persisted work must compare it before using elapsed-time metadata. */
    fun currentSessionToken(): Long = if (enabled) configuration.sessionToken ?: sessionToken() else 0L

    fun record(event: SyncTraceEvent, vararg fields: Pair<SyncTraceField, Long>) {
        val current = configuration
        if (!current.enabled) return
        try {
            val payload = buildString {
                append("event=").append(event.code)
                append(" session=").append(currentSessionToken())
                append(" timestamp_ms=").append(current.clock())
                append(" trace=").append(trace.get() ?: 0L)
                fields.forEach { (field, value) -> append(' ').append(field.code).append('=').append(value) }
            }
            current.sink(payload)
        } catch (_: Exception) {
            // Diagnostics must not affect capture or upload outcomes.
        }
    }

    fun <T> withTrace(traceId: Long, block: () -> T): T {
        if (!enabled) return block()
        val previous = trace.get()
        trace.set(traceId)
        return try {
            block()
        } finally {
            if (previous == null) trace.remove() else trace.set(previous)
        }
    }

    fun <T> measure(event: SyncTraceEvent, block: () -> T): T {
        if (!enabled) return block()
        val start = nowMs()
        val startCpu = configuration.cpuMs()
        return try {
            block().also { record(event, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to 1) }
        } catch (error: Throwable) {
            record(event, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to 0)
            throw error
        }
    }

    internal fun <T> measurePhase(phase: Long, block: () -> T): T {
        if (!enabled) return block()
        val start = nowMs(); val startCpu = configuration.cpuMs()
        return try {
            block().also { record(SyncTraceEvent.GATEWAY_PHASE, SyncTraceField.PHASE to phase, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to 1) }
        } catch (error: Throwable) {
            record(SyncTraceEvent.GATEWAY_PHASE, SyncTraceField.PHASE to phase, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to 0)
            throw error
        }
    }

    internal fun <T> measureNativeOpen(cold: Boolean, succeeded: (T) -> Boolean = { true }, block: () -> T): T {
        if (!enabled) return block()
        val start = nowMs(); val startCpu = configuration.cpuMs()
        return try {
            block().also { result -> record(SyncTraceEvent.NATIVE_OPEN, SyncTraceField.COLD to if (cold) 1 else 0, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to if (succeeded(result)) 1 else 0) }
        } catch (error: Throwable) {
            record(SyncTraceEvent.NATIVE_OPEN, SyncTraceField.COLD to if (cold) 1 else 0, SyncTraceField.DURATION_MS to (nowMs() - start), SyncTraceField.CPU_MS to (configuration.cpuMs() - startCpu), SyncTraceField.SUCCESS to 0)
            throw error
        }
    }

    @VisibleForTesting
    internal fun <T> withTestConfiguration(enabled: Boolean, clock: () -> Long, cpuMs: () -> Long, sink: (String) -> Unit, sessionToken: Long = 1L, block: () -> T): T {
        val previous = configuration
        configuration = Configuration(enabled, clock, cpuMs, sink, sessionToken)
        return try { block() } finally { configuration = previous; trace.remove() }
    }

    private fun sessionToken(): Long {
        if (session != 0L) return session
        synchronized(this) {
            if (session == 0L) session = SecureRandom().nextLong().and(Long.MAX_VALUE).coerceAtLeast(1)
            return session
        }
    }

    private const val TAG = "PeppySyncTrace"
    private const val NANOS_PER_MILLISECOND = 1_000_000L
}
