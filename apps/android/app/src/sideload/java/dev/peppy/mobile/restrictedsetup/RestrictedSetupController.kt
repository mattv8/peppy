package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

internal enum class RestrictedSetupStage { INTRODUCTION, AWAITING_PAIRING, PAIRING, CONNECTING, APPLYING, VERIFYING, TURNING_OFF, CLEANUP_REQUIRED, COMPLETE, FAILED }
internal enum class RestrictedSetupAccess { UNKNOWN, ALLOWED, NOT_ALLOWED }
internal enum class RestrictedSetupFailure { INVALID_ENDPOINT, CLEANUP_MARKER, TIMED_OUT, TRANSPORT, NOT_VERIFIED, PAIRING_FAILED, CONNECT_FAILED }
internal enum class RestrictedSetupDiscovery { SEARCHING, FOUND, TIMED_OUT }
internal enum class RestrictedSetupHint { NO_WIFI, CODE_REJECTED, PAIRING_BOX_CLOSED }
internal enum class WirelessDebuggingOutcome { NOT_REQUESTED, OFF, REQUESTED }
internal enum class CodeSubmitResult { ACCEPTED, INVALID_FORMAT, NO_PAIRING_PORT, NOT_WAITING }

internal data class RestrictedSetupOptions(val turnOffWirelessDebugging: Boolean = false)
internal data class RestrictedSetupState(
    val stage: RestrictedSetupStage,
    val access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN,
    val cleanupNeeded: Boolean,
    val failure: RestrictedSetupFailure? = null,
    val discovery: RestrictedSetupDiscovery? = null,
    val pairingPort: Int? = null,
    val notificationAvailable: Boolean = false,
    val hint: RestrictedSetupHint? = null,
    val turnOffWirelessDebugging: Boolean = false,
    val wirelessDebugging: WirelessDebuggingOutcome = WirelessDebuggingOutcome.NOT_REQUESTED,
) { val needsConnectPort get() = stage == RestrictedSetupStage.CONNECTING && discovery == RestrictedSetupDiscovery.TIMED_OUT }

internal data class RestrictedSetupDeadlines(
    val pairingServiceHintMillis: Long = 60_000,
    val pairingMillis: Long = 30_000,
    val connectServiceMillis: Long = 20_000,
    val connectMillis: Long = 30_000,
    val grantAndReadbackMillis: Long = 15_000,
    val turnOffMillis: Long = 10_000,
    val totalMillis: Long = 10 * 60_000,
)

internal fun interface RestrictedSetupAccessReadback { suspend fun isAllowed(): Boolean }
internal interface RestrictedSetupCleanupMarker { fun needsCleanup(): Boolean; fun markCleanupNeeded(): Boolean; fun acknowledgeCleanup(): Boolean }

/** Process-scoped state machine. Pairing codes and ADB keys are never persisted. */
internal class RestrictedSetupController(
    private val controllerScope: CoroutineScope,
    private val transportFactory: RestrictedSetupTransportFactory,
    private val accessReadback: RestrictedSetupAccessReadback,
    private val localAddresses: LocalAddressSupplier,
    private val cleanupMarker: RestrictedSetupCleanupMarker,
    private val discovery: AdbServiceDiscovery,
    private val notifier: RestrictedSetupNotifier,
    private val wirelessDebuggingReadback: RestrictedSetupWirelessReadback,
    private val deadlines: RestrictedSetupDeadlines = RestrictedSetupDeadlines(),
    private val watchdogScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
) : AutoCloseable {
    private val lock = Any()
    private var epoch = 0L
    private var active: Attempt? = null
    private var teardownFailed = false
    private val initialCleanup = cleanupMarker.needsCleanup()
    private val mutableState = MutableStateFlow(RestrictedSetupState(if (initialCleanup) RestrictedSetupStage.CLEANUP_REQUIRED else RestrictedSetupStage.INTRODUCTION, cleanupNeeded = initialCleanup))
    val state: StateFlow<RestrictedSetupState> = mutableState.asStateFlow()

    fun begin(options: RestrictedSetupOptions): Boolean = synchronized(lock) {
        if (teardownFailed || active != null || mutableState.value.stage !in setOf(RestrictedSetupStage.INTRODUCTION, RestrictedSetupStage.FAILED, RestrictedSetupStage.CLEANUP_REQUIRED)) return false
        val candidate = Attempt(++epoch, options, notifier.canPrompt())
        active = candidate
        mutableState.value = RestrictedSetupState(
            stage = RestrictedSetupStage.AWAITING_PAIRING,
            cleanupNeeded = cleanupMarker.needsCleanup(),
            discovery = RestrictedSetupDiscovery.SEARCHING,
            notificationAvailable = candidate.notificationAvailable,
            turnOffWirelessDebugging = options.turnOffWirelessDebugging,
        )
        RestrictedSetupCodeInbox.sink = { code -> submitCode(code) }
        notifier.clear()
        notifier.show(RestrictedSetupNotice.SEARCHING)
        candidate.totalWatchdog = watchdogScope.launch { delay(deadlines.totalMillis); timeout(candidate) }
        candidate.pairingDiscovery = controllerScope.launch { collectPairing(candidate) }
        candidate.addressCheck = controllerScope.launch {
            runCatching { localAddresses.currentAddresses() }.getOrNull()
                ?.takeIf { addresses -> addresses.none { !it.isLoopbackAddress } }
                ?.let { update(candidate) { state -> if (state.stage == RestrictedSetupStage.AWAITING_PAIRING && state.hint == null) state.copy(hint = RestrictedSetupHint.NO_WIFI) else state } }
        }
        true
    }

    fun submitCode(code: CharArray, manualPort: Int? = null): CodeSubmitResult {
        if (!code.all { it in '0'..'9' } || code.size != 6) {
            code.fill('\u0000')
            synchronized(lock) { active?.takeIf { mutableState.value.stage == RestrictedSetupStage.AWAITING_PAIRING } }?.let { notifier.show(RestrictedSetupNotice.CODE_INVALID) }
            return CodeSubmitResult.INVALID_FORMAT
        }
        val candidate: Attempt
        val port: Int
        synchronized(lock) {
            candidate = active ?: return code.zeroed(CodeSubmitResult.NOT_WAITING)
            if (mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING || candidate.procedure != null) return code.zeroed(CodeSubmitResult.NOT_WAITING)
            port = manualPort ?: mutableState.value.pairingPort ?: return code.zeroed(CodeSubmitResult.NO_PAIRING_PORT)
            candidate.code = code
            mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.PAIRING, discovery = null, hint = null)
            RestrictedSetupCodeInbox.sink = null
            notifier.show(RestrictedSetupNotice.WORKING)
            candidate.procedure = controllerScope.launch { pairThenConnect(candidate, port, manualPort == null) }
        }
        return CodeSubmitResult.ACCEPTED
    }

    fun submitConnectPort(port: Int): Boolean {
        val candidate = synchronized(lock) {
            val current = active
            if (current == null || !mutableState.value.needsConnectPort || current.procedure != null || current.connecting) null else current.also {
                it.connecting = true
                it.procedure = controllerScope.launch { connectAndApply(it, port, stopDiscovery = true) }
            }
        }
        return candidate != null
    }

    fun showCodePromptAgain() {
        synchronized(lock) { mutableState.value }.takeIf { it.stage == RestrictedSetupStage.AWAITING_PAIRING && it.discovery == RestrictedSetupDiscovery.FOUND && it.notificationAvailable }?.let { notifier.show(RestrictedSetupNotice.CODE_PROMPT) }
    }

    fun clearNotice() { if (state.value.stage != RestrictedSetupStage.AWAITING_PAIRING) notifier.clear() }

    fun reportForegroundUnavailable() {
        val candidate = synchronized(lock) { active } ?: return
        notifier.clear()
        if (terminal(candidate, RestrictedSetupStage.FAILED, RestrictedSetupFailure.TRANSPORT, RestrictedSetupAccess.UNKNOWN, WirelessDebuggingOutcome.NOT_REQUESTED)) {
            notifier.show(RestrictedSetupNotice.FAILED)
        }
    }

    fun cancel() {
        val candidate = synchronized(lock) {
            val current = active
            epoch++
            active = null
            RestrictedSetupCodeInbox.sink = null
            mutableState.value = mutableState.value.copy(stage = if (cleanupMarker.needsCleanup()) RestrictedSetupStage.CLEANUP_REQUIRED else RestrictedSetupStage.INTRODUCTION, cleanupNeeded = cleanupMarker.needsCleanup(), failure = null, discovery = null, pairingPort = null, hint = null)
            current
        }
        notifier.clear()
        candidate?.cancel()
    }

    fun acknowledgeCleanup(): Boolean = synchronized(lock) {
        if (teardownFailed || active != null || !cleanupMarker.acknowledgeCleanup()) false else {
            mutableState.value = RestrictedSetupState(RestrictedSetupStage.INTRODUCTION, cleanupNeeded = false)
            true
        }
    }

    override fun close() = cancel()

    private suspend fun collectPairing(candidate: Attempt) {
        val hint = controllerScope.launch { delay(deadlines.pairingServiceHintMillis); update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.TIMED_OUT) } }
        try {
            discovery.ports(AdbServiceKind.PAIRING).collect { port ->
                if (port == null) {
                    if (candidate.hadPairingPort) {
                        candidate.hadPairingPort = false
                        update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.SEARCHING, pairingPort = null, hint = RestrictedSetupHint.PAIRING_BOX_CLOSED) }
                        notifier.show(RestrictedSetupNotice.SEARCHING)
                    }
                } else {
                    candidate.hadPairingPort = true
                    hint.cancel()
                    update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.FOUND, pairingPort = port, hint = null) }
                    if (candidate.notificationAvailable) notifier.show(RestrictedSetupNotice.CODE_PROMPT)
                }
            }
        } finally { hint.cancel() }
    }

    private suspend fun pairThenConnect(candidate: Attempt, requestedPort: Int, discoveredPort: Boolean) {
        try {
            if (discoveredPort && state.value.pairingPort != requestedPort) throw SetupException(RestrictedSetupFailure.PAIRING_FAILED)
            val endpoint = endpoint(requestedPort)
            if (!cleanupMarker.markCleanupNeeded()) throw SetupException(RestrictedSetupFailure.CLEANUP_MARKER)
            val transport = transportFactory.create().also { attach(candidate, it) }
            withDeadline(candidate, deadlines.pairingMillis) { transport.pair(endpoint, candidate.code!!) }
            candidate.code?.fill('\u0000'); candidate.code = null
            update(candidate) { it.copy(stage = RestrictedSetupStage.CONNECTING, discovery = RestrictedSetupDiscovery.SEARCHING, pairingPort = null) }
            candidate.pairingDiscovery?.cancelAndJoin()
            candidate.connectDiscovery = controllerScope.launch { collectConnect(candidate) }
            synchronized(lock) { if (active === candidate) candidate.procedure = null }
        } catch (_: PairingRejectedException) {
            candidate.code?.fill('\u0000'); candidate.code = null
            candidate.resetTransport()
            candidate.hadPairingPort = false
            val retrying = synchronized(lock) {
                if (active !== candidate || candidate.epoch != epoch) {
                    false
                } else {
                    mutableState.value = mutableState.value.copy(
                        stage = RestrictedSetupStage.AWAITING_PAIRING,
                        discovery = RestrictedSetupDiscovery.SEARCHING,
                        pairingPort = null,
                        hint = RestrictedSetupHint.CODE_REJECTED,
                    )
                    RestrictedSetupCodeInbox.sink = { code -> submitCode(code) }
                    candidate.procedure = null
                    true
                }
            }
            if (retrying) notifier.show(RestrictedSetupNotice.CODE_REJECTED)
        } catch (error: CancellationException) { throw error }
        catch (error: SetupException) { fail(candidate, error.failure) }
        catch (_: Exception) { fail(candidate, RestrictedSetupFailure.PAIRING_FAILED) }
    }

    private suspend fun collectConnect(candidate: Attempt) {
        val hint = controllerScope.launch { delay(deadlines.connectServiceMillis); update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.TIMED_OUT) } }
        try {
            discovery.ports(AdbServiceKind.CONNECT).collect { port ->
                if (port != null && beginConnection(candidate)) {
                    hint.cancel()
                    update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.FOUND) }
                    candidate.procedure = controllerScope.launch { connectAndApply(candidate, port, stopDiscovery = false) }
                    candidate.connectDiscovery?.cancel()
                }
            }
        } finally { hint.cancel() }
    }

    private suspend fun connectAndApply(candidate: Attempt, port: Int, stopDiscovery: Boolean) {
        try {
            val transport = candidate.transport ?: throw SetupException(RestrictedSetupFailure.CONNECT_FAILED)
            withDeadline(candidate, deadlines.connectMillis) { transport.connect(endpoint(port)) }
            if (stopDiscovery) candidate.connectDiscovery?.cancel()
            update(candidate) { it.copy(stage = RestrictedSetupStage.APPLYING, discovery = null) }
            try { withDeadline(candidate, deadlines.grantAndReadbackMillis) { transport.grantOwnRestrictedAccess() } } catch (error: CancellationException) { throw error } catch (_: Exception) { }
            verify(candidate)
        } catch (error: CancellationException) { throw error }
        catch (error: SetupException) { fail(candidate, error.failure) }
        catch (_: Exception) { fail(candidate, RestrictedSetupFailure.CONNECT_FAILED) }
    }

    private suspend fun verify(candidate: Attempt) {
        update(candidate) { it.copy(stage = RestrictedSetupStage.VERIFYING) }
        val allowed = try { withTimeout(deadlines.grantAndReadbackMillis) { accessReadback.isAllowed() } } catch (error: CancellationException) { throw error } catch (_: Exception) { fail(candidate, RestrictedSetupFailure.TIMED_OUT); return }
        if (!allowed) { fail(candidate, RestrictedSetupFailure.NOT_VERIFIED, RestrictedSetupAccess.NOT_ALLOWED); return }
        if (!candidate.options.turnOffWirelessDebugging) { complete(candidate, WirelessDebuggingOutcome.NOT_REQUESTED); return }
        update(candidate) { it.copy(stage = RestrictedSetupStage.TURNING_OFF, access = RestrictedSetupAccess.ALLOWED) }
        val outcome = try { withDeadline(candidate, deadlines.turnOffMillis) { candidate.transport!!.turnWirelessDebuggingOff() }; if (wirelessDebuggingReadback.isOn() == false) WirelessDebuggingOutcome.OFF else WirelessDebuggingOutcome.REQUESTED } catch (error: CancellationException) { throw error } catch (_: Exception) { WirelessDebuggingOutcome.REQUESTED }
        complete(candidate, outcome)
    }

    private suspend fun endpoint(port: Int): LocalEndpoint {
        val addresses = withTimeout(deadlines.pairingMillis) { localAddresses.currentAddresses() }
        return LocalEndpoint.parse("127.0.0.1", port, addresses) ?: throw SetupException(RestrictedSetupFailure.INVALID_ENDPOINT)
    }

    private suspend fun <T> withDeadline(candidate: Attempt, timeout: Long, action: suspend () -> T): T {
        val watchdog = watchdogScope.launch { delay(timeout); candidate.closeTransport() }
        return try { withTimeout(timeout) { withContext(Dispatchers.IO) { action() } } } finally { watchdog.cancel() }
    }

    private fun timeout(candidate: Attempt) { candidate.closeTransport(); fail(candidate, RestrictedSetupFailure.TIMED_OUT); candidate.cancel() }
    private fun attach(candidate: Attempt, transport: RestrictedSetupTransport) { synchronized(lock) { if (active === candidate) candidate.transport = transport else transport.close() } }
    private fun complete(candidate: Attempt, wireless: WirelessDebuggingOutcome) { if (terminal(candidate, RestrictedSetupStage.COMPLETE, null, RestrictedSetupAccess.ALLOWED, wireless)) notifier.show(RestrictedSetupNotice.DONE) }
    private fun fail(candidate: Attempt, failure: RestrictedSetupFailure, access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN) { if (terminal(candidate, RestrictedSetupStage.FAILED, failure, access, WirelessDebuggingOutcome.NOT_REQUESTED)) notifier.show(RestrictedSetupNotice.FAILED) }
    private fun terminal(candidate: Attempt, stage: RestrictedSetupStage, failure: RestrictedSetupFailure?, access: RestrictedSetupAccess, wireless: WirelessDebuggingOutcome): Boolean {
        val terminal = synchronized(lock) {
            if (active !== candidate || candidate.epoch != epoch) return@synchronized false
            mutableState.value = mutableState.value.copy(stage = stage, failure = failure, access = access, discovery = null, pairingPort = null, wirelessDebugging = wireless, cleanupNeeded = cleanupMarker.needsCleanup())
            RestrictedSetupCodeInbox.sink = null
            true
        }
        if (!terminal) return false
        candidate.cancel()
        return true
    }
    private fun beginConnection(candidate: Attempt): Boolean = synchronized(lock) {
        if (active !== candidate || candidate.epoch != epoch || candidate.connecting) false else { candidate.connecting = true; true }
    }
    private fun update(candidate: Attempt, transform: (RestrictedSetupState) -> RestrictedSetupState) = synchronized(lock) { if (active === candidate && candidate.epoch == epoch) mutableState.value = transform(mutableState.value) }
    private fun CharArray.zeroed(result: CodeSubmitResult): CodeSubmitResult { fill('\u0000'); return result }
    private class SetupException(val failure: RestrictedSetupFailure) : Exception()

    private inner class Attempt(val epoch: Long, val options: RestrictedSetupOptions, val notificationAvailable: Boolean) {
        var code: CharArray? = null
        var transport: RestrictedSetupTransport? = null
        var pairingDiscovery: Job? = null
        var connectDiscovery: Job? = null
        var procedure: Job? = null
        var totalWatchdog: Job? = null
        var addressCheck: Job? = null
        var hadPairingPort = false
        var connecting = false
        private val closed = AtomicBoolean(false)
        fun cancel() { code?.fill('\u0000'); code = null; pairingDiscovery?.cancel(); connectDiscovery?.cancel(); procedure?.cancel(); totalWatchdog?.cancel(); addressCheck?.cancel(); closeTransport(); synchronized(lock) { if (active === this) active = null } }
        fun resetTransport() { closeTransport(); transport = null; closed.set(false) }
        fun closeTransport() { val value = transport ?: return; if (closed.compareAndSet(false, true)) watchdogScope.launch(Dispatchers.IO) { val success = runCatching { value.close() }.isSuccess; if (!success) synchronized(lock) { teardownFailed = true; if (this@RestrictedSetupController.epoch == this@Attempt.epoch) mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.CLEANUP_REQUIRED, cleanupNeeded = true, failure = RestrictedSetupFailure.TRANSPORT) } } }
    }
}
