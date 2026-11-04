package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

internal enum class RestrictedSetupStage {
    INTRODUCTION, PREPARATION, AWAITING_PAIRING, PAIRING, CONNECTING, APPLYING, VERIFYING, CLEANUP_REQUIRED, COMPLETE, FAILED,
}

internal enum class RestrictedSetupAccess { UNKNOWN, ALLOWED, NOT_ALLOWED }

internal enum class RestrictedSetupFailure { INVALID_ENDPOINT, CLEANUP_MARKER, TIMED_OUT, TRANSPORT, NOT_VERIFIED }

internal data class RestrictedSetupState(
    val stage: RestrictedSetupStage,
    val access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN,
    val cleanupNeeded: Boolean,
    val failure: RestrictedSetupFailure? = null,
)

internal data class RestrictedSetupDeadlines(
    val discoveryMillis: Long = 30_000,
    val pairingMillis: Long = 30_000,
    val connectMillis: Long = 30_000,
    val grantAndReadbackMillis: Long = 15_000,
    val totalMillis: Long = 10 * 60_000,
)

internal fun interface RestrictedSetupAccessReadback { suspend fun isAllowed(): Boolean }

internal interface RestrictedSetupCleanupMarker {
    fun needsCleanup(): Boolean
    fun markCleanupNeeded(): Boolean
    fun acknowledgeCleanup(): Boolean
}

/** ViewModel-owned state machine. It never persists pairing credentials or command output. */
internal class RestrictedSetupController(
    private val controllerScope: CoroutineScope,
    private val transportFactory: RestrictedSetupTransportFactory,
    private val accessReadback: RestrictedSetupAccessReadback,
    private val localAddresses: LocalAddressSupplier,
    private val cleanupMarker: RestrictedSetupCleanupMarker,
    private val deadlines: RestrictedSetupDeadlines = RestrictedSetupDeadlines(),
    private val watchdogScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
) : AutoCloseable {
    private val lock = Any()
    private var nextEpoch = 0L
    private var attempt: Attempt? = null
    private var teardownFailed = false
    private val initialCleanupNeeded = cleanupMarker.needsCleanup()
    private val mutableState = MutableStateFlow(
        RestrictedSetupState(
            stage = if (initialCleanupNeeded) RestrictedSetupStage.CLEANUP_REQUIRED else RestrictedSetupStage.INTRODUCTION,
            cleanupNeeded = initialCleanupNeeded,
        ),
    )
    val state: StateFlow<RestrictedSetupState> = mutableState.asStateFlow()

    fun prepare(): Boolean = synchronized(lock) {
        if (teardownFailed || attempt != null || mutableState.value.stage !in setOf(RestrictedSetupStage.INTRODUCTION, RestrictedSetupStage.FAILED, RestrictedSetupStage.CLEANUP_REQUIRED)) {
            return false
        }
        mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.PREPARATION, failure = null)
        true
    }

    fun awaitPairing(): Boolean = synchronized(lock) {
        if (teardownFailed || attempt != null || mutableState.value.stage != RestrictedSetupStage.PREPARATION) return false
        mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.AWAITING_PAIRING)
        true
    }

    /** Takes ownership of [code] until the attempt completion hook zeros it. */
    @OptIn(InternalCoroutinesApi::class)
    fun submit(pairingEndpoint: LocalEndpoint, connectEndpoint: LocalEndpoint, code: CharArray): Boolean = synchronized(lock) {
        if (teardownFailed || attempt != null || mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING) return false
        val candidate = Attempt(++nextEpoch)
        candidate.code = code
        attempt = candidate
        val job = controllerScope.launch(start = CoroutineStart.LAZY) { runAttempt(candidate, pairingEndpoint, connectEndpoint) }
        candidate.job = job
        job.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { finishAttempt(candidate, code) }
        job.start()
        true
    }

    fun cancel() {
        val cancelled = synchronized(lock) {
            val active = attempt
            if (active != null) {
                active.cancelled = true
                nextEpoch += 1
            }
            mutableState.value = mutableState.value.copy(
                stage = if (cleanupMarker.needsCleanup()) RestrictedSetupStage.CLEANUP_REQUIRED else RestrictedSetupStage.INTRODUCTION,
                cleanupNeeded = cleanupMarker.needsCleanup(),
                failure = null,
            )
            active
        }
        cancelled?.transport?.let { requestClose(cancelled, it) }
        cancelled?.job?.cancel()
    }

    fun acknowledgeCleanup(): Boolean = synchronized(lock) {
        if (teardownFailed || attempt != null || !cleanupMarker.acknowledgeCleanup()) return false
        mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.INTRODUCTION, cleanupNeeded = false)
        true
    }

    override fun close() = cancel()

    private suspend fun runAttempt(attempt: Attempt, pairingEndpoint: LocalEndpoint, connectEndpoint: LocalEndpoint) {
        val totalWatchdog = watchdogScope.launch(Dispatchers.IO) {
            delay(deadlines.totalMillis)
            attempt.totalExpired.set(true)
            closeForTimeout(attempt)
        }
        try {
            withTimeout(deadlines.totalMillis) {
                setStage(attempt.epoch, RestrictedSetupStage.PAIRING)
                validate(pairingEndpoint, withTimeout(deadlines.discoveryMillis) { localAddresses.currentAddresses() })
                if (!cleanupMarker.markCleanupNeeded()) throw SetupException(RestrictedSetupFailure.CLEANUP_MARKER)
                val transport = transportFactory.create()
                if (!attachTransport(attempt, transport)) return@withTimeout
                withTransportDeadline(attempt, deadlines.pairingMillis) { transport.pair(pairingEndpoint, attempt.code) }
                ensureNotExpired(attempt)

                setStage(attempt.epoch, RestrictedSetupStage.CONNECTING)
                validate(connectEndpoint, withTimeout(deadlines.discoveryMillis) { localAddresses.currentAddresses() })
                withTransportDeadline(attempt, deadlines.connectMillis) { transport.connect(connectEndpoint) }
                ensureNotExpired(attempt)

                setStage(attempt.epoch, RestrictedSetupStage.APPLYING)
                try {
                    withTransportDeadline(attempt, deadlines.grantAndReadbackMillis) { transport.grantOwnRestrictedAccess() }
                } catch (_: OperationTimedOutException) {
                    verifyAfterUnknownGrant(attempt)
                    return@withTimeout
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    verifyAfterUnknownGrant(attempt)
                    return@withTimeout
                }
                verifyAfterUnknownGrant(attempt)
            }
        } catch (_: OperationTimedOutException) {
            fail(attempt.epoch, RestrictedSetupFailure.TIMED_OUT)
        } catch (_: TimeoutCancellationException) {
            fail(attempt.epoch, RestrictedSetupFailure.TIMED_OUT)
        } catch (_: CancellationException) {
            // Parent cancellation owns its lifecycle; do not publish a state transition.
        } catch (error: SetupException) {
            fail(attempt.epoch, error.failure)
        } catch (_: Exception) {
            fail(attempt.epoch, RestrictedSetupFailure.TRANSPORT)
        } finally {
            totalWatchdog.cancel()
        }
    }

    private suspend fun verifyAfterUnknownGrant(attempt: Attempt) {
        setStage(attempt.epoch, RestrictedSetupStage.VERIFYING)
        ensureActive(attempt)
        val allowed = try {
            withTimeout(deadlines.grantAndReadbackMillis) { accessReadback.isAllowed() }
        } catch (_: TimeoutCancellationException) {
            fail(attempt.epoch, RestrictedSetupFailure.TIMED_OUT)
            return
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            fail(attempt.epoch, RestrictedSetupFailure.TRANSPORT)
            return
        }
        if (allowed) complete(attempt.epoch) else fail(attempt.epoch, RestrictedSetupFailure.NOT_VERIFIED, RestrictedSetupAccess.NOT_ALLOWED)
    }

    private suspend fun <T> withTransportDeadline(attempt: Attempt, timeoutMillis: Long, action: suspend () -> T): T {
        ensureActive(attempt)
        val expired = AtomicBoolean(false)
        val watchdog = watchdogScope.launch(Dispatchers.IO) {
            delay(timeoutMillis)
            expired.set(true)
            closeForTimeout(attempt)
        }
        return try {
            val result = withTimeout(timeoutMillis) {
                withContext(Dispatchers.IO) {
                    ensureActive(attempt)
                    action()
                }
            }
            if (expired.get()) throw OperationTimedOutException()
            result
        } catch (_: TimeoutCancellationException) {
            throw OperationTimedOutException()
        } finally {
            watchdog.cancel()
        }
    }

    private fun closeForTimeout(candidate: Attempt) {
        val transport = synchronized(lock) { if (attempt === candidate) candidate.transport else null }
        if (transport != null) requestClose(candidate, transport)
    }

    private fun ensureNotExpired(candidate: Attempt) {
        if (candidate.totalExpired.get()) throw OperationTimedOutException()
    }

    private fun ensureActive(candidate: Attempt) {
        if (synchronized(lock) { attempt !== candidate || candidate.cancelled }) throw CancellationException()
    }

    private fun validate(endpoint: LocalEndpoint, currentAddresses: Set<InetAddress>) {
        if (!endpoint.isCurrentLocalAddress(currentAddresses)) throw SetupException(RestrictedSetupFailure.INVALID_ENDPOINT)
    }

    private fun attachTransport(candidate: Attempt, transport: RestrictedSetupTransport): Boolean {
        val attached = synchronized(lock) {
            if (attempt !== candidate || candidate.cancelled) false else {
            candidate.transport = transport
            true
            }
        }
        if (!attached) closeUntracked(transport)
        return attached
    }

    private fun finishAttempt(completed: Attempt, code: CharArray) {
        code.fill('\u0000')
        val transport = synchronized(lock) {
            completed.completionObserved = true
            completed.transport
        }
        transport?.let { requestClose(completed, it) } ?: finishTeardown(completed, true)
        detachAfterTeardown(completed)
    }

    private fun requestClose(candidate: Attempt, transport: RestrictedSetupTransport) {
        val shouldClose = synchronized(lock) {
            if (candidate.closeRequested) false else {
                candidate.closeRequested = true
                true
            }
        }
        if (!shouldClose) return
        watchdogScope.launch(Dispatchers.IO) {
            finishTeardown(candidate, runCatching { transport.close() }.isSuccess)
        }
    }

    private fun closeUntracked(transport: RestrictedSetupTransport) {
        watchdogScope.launch(Dispatchers.IO) { runCatching { transport.close() } }
    }

    private fun finishTeardown(candidate: Attempt, closed: Boolean) = synchronized(lock) {
        candidate.closeFinished = true
        if (attempt === candidate && !closed) {
            teardownFailed = true
            mutableState.value = mutableState.value.copy(
                stage = RestrictedSetupStage.CLEANUP_REQUIRED,
                cleanupNeeded = true,
                failure = RestrictedSetupFailure.TRANSPORT,
            )
        }
        detachAfterTeardown(candidate)
    }

    private fun detachAfterTeardown(candidate: Attempt) {
        synchronized(lock) {
            if (attempt === candidate && candidate.completionObserved && candidate.closeFinished) attempt = null
        }
    }

    private fun setStage(epoch: Long, stage: RestrictedSetupStage) = update(epoch) {
        it.copy(stage = stage, failure = null, cleanupNeeded = cleanupMarker.needsCleanup())
    }

    private fun complete(epoch: Long) = update(epoch) {
        it.copy(stage = RestrictedSetupStage.COMPLETE, access = RestrictedSetupAccess.ALLOWED, cleanupNeeded = cleanupMarker.needsCleanup(), failure = null)
    }

    private fun fail(epoch: Long, failure: RestrictedSetupFailure, access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN) = update(epoch) {
        it.copy(stage = RestrictedSetupStage.FAILED, access = access, cleanupNeeded = cleanupMarker.needsCleanup(), failure = failure)
    }

    private inline fun update(epoch: Long, transform: (RestrictedSetupState) -> RestrictedSetupState) = synchronized(lock) {
        val active = attempt
        if (active?.epoch == epoch && !active.cancelled) mutableState.value = transform(mutableState.value)
    }

    private class Attempt(val epoch: Long) {
        lateinit var code: CharArray
        val totalExpired = AtomicBoolean(false)
        var job: Job? = null
        var transport: RestrictedSetupTransport? = null
        var cancelled = false
        var closeRequested = false
        var closeFinished = false
        var completionObserved = false
    }

    private class SetupException(val failure: RestrictedSetupFailure) : Exception()
    private class OperationTimedOutException : Exception()
}
