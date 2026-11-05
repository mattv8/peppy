package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import dev.peppy.mobile.mms.RestrictedMessageAccess

internal enum class RestrictedSetupStage { INTRODUCTION, AWAITING_PAIRING, PAIRING, CONNECTING, APPLYING, VERIFYING, TURNING_OFF, COMPLETE, FAILED }
internal enum class RestrictedSetupAccess { UNKNOWN, ALLOWED, NOT_ALLOWED, UNSUPPORTED }
internal enum class RestrictedSetupFailure { INVALID_ENDPOINT, TIMED_OUT, TRANSPORT, NOT_VERIFIED, PAIRING_FAILED, CONNECT_FAILED }
internal enum class RestrictedSetupDiscovery { SEARCHING, FOUND, TIMED_OUT }
internal enum class RestrictedSetupHint { NO_WIFI, NETWORK_NOT_ALLOWED, CODE_REJECTED, PAIRING_BOX_CLOSED }
internal enum class WirelessDebuggingOutcome { NOT_REQUESTED, OFF, REQUESTED }
internal enum class CodeSubmitResult { ACCEPTED, INVALID_FORMAT, NO_PAIRING_PORT, NOT_WAITING }

internal data class RestrictedSetupReadback(
    val restrictedMessages: RestrictedSetupAccess,
    val blockedSenders: RestrictedSetupAccess,
)
internal data class RestrictedSetupState(
    val stage: RestrictedSetupStage,
    val access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN,
    val blockedSendersAccess: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN,
    val failure: RestrictedSetupFailure? = null,
    val discovery: RestrictedSetupDiscovery? = null,
    val pairingPort: Int? = null,
    val notificationAvailable: Boolean = false,
    val hint: RestrictedSetupHint? = null,
    val networkNotAllowed: Boolean = false,
    val wirelessDebugging: WirelessDebuggingOutcome = WirelessDebuggingOutcome.NOT_REQUESTED,
    val retryBlocked: Boolean = false,
) {
    val needsConnectPort get() = stage == RestrictedSetupStage.CONNECTING && discovery == RestrictedSetupDiscovery.TIMED_OUT
    /** Hint precedence: NO_WIFI > NETWORK_NOT_ALLOWED > CODE_REJECTED/PAIRING_BOX_CLOSED. */
    val activeHint get() = if (hint == RestrictedSetupHint.NO_WIFI) hint else if (networkNotAllowed) RestrictedSetupHint.NETWORK_NOT_ALLOWED else hint
}

private const val TRANSPORT_CLOSE_GRACE_MILLIS = 5_000L

internal data class RestrictedSetupDeadlines(
    val pairingServiceHintMillis: Long = 60_000,
    val networkAllowedHintMillis: Long = 5_000,
    val networkAllowedHintCheckMillis: Long = 3_000,
    val pairingMillis: Long = 30_000,
    val connectServiceMillis: Long = 20_000,
    val connectMillis: Long = 30_000,
    val grantAndReadbackMillis: Long = 15_000,
    val turnOffMillis: Long = 10_000,
    val totalMillis: Long = 10 * 60_000,
)

internal fun interface RestrictedSetupAccessReadback { suspend fun read(): RestrictedSetupReadback }
/** Process-scoped state machine. Pairing codes and ADB keys are never persisted. */
internal class RestrictedSetupController(
    private val controllerScope: CoroutineScope,
    private val transportFactory: RestrictedSetupTransportFactory,
    private val accessReadback: RestrictedSetupAccessReadback,
    private val localAddresses: LocalAddressSupplier,
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
    private val mutableState = MutableStateFlow(RestrictedSetupState(RestrictedSetupStage.INTRODUCTION))
    val state: StateFlow<RestrictedSetupState> = mutableState.asStateFlow()

    fun begin(): Boolean = synchronized(lock) {
        if (teardownFailed || active != null || mutableState.value.stage !in setOf(RestrictedSetupStage.INTRODUCTION, RestrictedSetupStage.FAILED)) return false
        val candidate = Attempt(++epoch, notifier.canPrompt())
        active = candidate
        mutableState.value = RestrictedSetupState(
            stage = RestrictedSetupStage.AWAITING_PAIRING,
            discovery = RestrictedSetupDiscovery.SEARCHING,
            notificationAvailable = candidate.notificationAvailable,
        )
        RestrictedSetupCodeInbox.sink = { code -> submitCode(code) }
        notifier.clear()
        post(candidate, RestrictedSetupNotice.SEARCHING)
        candidate.totalWatchdog = watchdogScope.launch { delay(deadlines.totalMillis); timeout(candidate) }
        candidate.pairingDiscovery = controllerScope.launch { collectPairing(candidate) }
        candidate.connectPresenceDiscovery = controllerScope.launch { collectConnectPresence(candidate) }
        candidate.networkHintCheck = controllerScope.launch { checkNetworkAllowed(candidate) }
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
            synchronized(lock) { active?.takeIf { mutableState.value.stage == RestrictedSetupStage.AWAITING_PAIRING } }?.let { post(it, RestrictedSetupNotice.CODE_INVALID, force = true) }
            return CodeSubmitResult.INVALID_FORMAT
        }
        val candidate: Attempt
        val port: Int
        synchronized(lock) {
            candidate = active ?: return code.zeroed(CodeSubmitResult.NOT_WAITING)
            if (mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING || candidate.procedure != null) return code.zeroed(CodeSubmitResult.NOT_WAITING)
            port = manualPort ?: mutableState.value.pairingPort ?: return code.zeroed(CodeSubmitResult.NO_PAIRING_PORT)
            candidate.code = code
            mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.PAIRING, discovery = null, hint = null, networkNotAllowed = false)
            candidate.connectPresenceDiscovery?.cancel()
            candidate.networkHintCheck?.cancel()
            RestrictedSetupCodeInbox.sink = null
            post(candidate, RestrictedSetupNotice.WORKING)
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
        synchronized(lock) { active to mutableState.value }.takeIf { (_, state) -> state.stage == RestrictedSetupStage.AWAITING_PAIRING && state.discovery == RestrictedSetupDiscovery.FOUND && state.notificationAvailable }?.first?.let { post(it, RestrictedSetupNotice.CODE_PROMPT, force = true) }
    }

    fun clearNotice() { if (state.value.stage != RestrictedSetupStage.AWAITING_PAIRING) notifier.clear() }

    fun reportForegroundUnavailable() {
        val candidate = synchronized(lock) { active } ?: return
        notifier.clear()
        if (terminal(candidate, RestrictedSetupStage.FAILED, RestrictedSetupFailure.TRANSPORT, RestrictedSetupAccess.UNKNOWN, RestrictedSetupAccess.UNKNOWN, WirelessDebuggingOutcome.NOT_REQUESTED)) {
            post(candidate, RestrictedSetupNotice.FAILED)
        }
    }

    fun cancel() {
        val candidate = synchronized(lock) {
            val current = active
            epoch++
            active = null
            RestrictedSetupCodeInbox.sink = null
            mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.INTRODUCTION, failure = null, discovery = null, pairingPort = null, hint = null, networkNotAllowed = false)
            current
        }
        notifier.clear()
        candidate?.cancel()
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
                        post(candidate, RestrictedSetupNotice.SEARCHING)
                    }
                } else {
                    candidate.hadPairingPort = true
                    hint.cancel()
                    update(candidate) { it.copy(discovery = RestrictedSetupDiscovery.FOUND, pairingPort = port, hint = null) }
                    if (candidate.notificationAvailable) post(candidate, RestrictedSetupNotice.CODE_PROMPT)
                }
            }
        } finally { hint.cancel() }
    }

    private suspend fun collectConnectPresence(candidate: Attempt) {
        discovery.ports(AdbServiceKind.CONNECT).collect { port ->
            candidate.hasConnectService = port != null
            if (port != null) clearNetworkNotAllowedHint(candidate)
        }
    }

    private suspend fun checkNetworkAllowed(candidate: Attempt) {
        while (true) {
            val wirelessOn = wirelessDebuggingReadback.isOn() == true
            val now = System.nanoTime() / 1_000_000
            val changed = synchronized(lock) {
                if (active !== candidate || candidate.epoch != epoch || mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING) {
                    false
                } else {
                    if (!wirelessOn) {
                        candidate.onSinceMillis = null
                        candidate.negativeChecks = 0
                    } else {
                        if (candidate.onSinceMillis == null) candidate.onSinceMillis = now
                        if (!candidate.hasConnectService) candidate.negativeChecks++ else candidate.negativeChecks = 0
                    }
                    val networkNotAllowed = wirelessOn && !candidate.hasConnectService && candidate.negativeChecks >= 2 && now - (candidate.onSinceMillis ?: now) >= deadlines.networkAllowedHintMillis
                    if (mutableState.value.networkNotAllowed == networkNotAllowed) false
                    else {
                        mutableState.value = mutableState.value.copy(networkNotAllowed = networkNotAllowed)
                        true
                    }
                }
            }
            if (changed) {
                if (state.value.networkNotAllowed && state.value.hint != RestrictedSetupHint.NO_WIFI) post(candidate, RestrictedSetupNotice.NETWORK_NOT_ALLOWED)
                else showAwaitingNotice(candidate)
            }
            delay(deadlines.networkAllowedHintCheckMillis)
        }
    }

    private fun showAwaitingNotice(candidate: Attempt) {
        val notice = synchronized(lock) {
            if (active !== candidate || mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING) null
            else if (candidate.notificationAvailable && mutableState.value.pairingPort != null) RestrictedSetupNotice.CODE_PROMPT
            else RestrictedSetupNotice.SEARCHING
        }
        notice?.let { post(candidate, it) }
    }

    private fun clearNetworkNotAllowedHint(candidate: Attempt) {
        val cleared = synchronized(lock) {
            if (active !== candidate || mutableState.value.stage != RestrictedSetupStage.AWAITING_PAIRING || !mutableState.value.networkNotAllowed) false
            else {
                mutableState.value = mutableState.value.copy(networkNotAllowed = false)
                true
            }
        }
        if (cleared) showAwaitingNotice(candidate)
    }

    private fun post(candidate: Attempt, notice: RestrictedSetupNotice, force: Boolean = false) {
        val shouldPost = synchronized(lock) {
            val outcome = notice == RestrictedSetupNotice.DONE || notice == RestrictedSetupNotice.FAILED
            if (!outcome && (active !== candidate || candidate.epoch != epoch)) false
            else if (!force && candidate.lastNotice == notice) false else {
                candidate.lastNotice = notice
                true
            }
        }
        if (shouldPost) notifier.show(notice)
    }

    private suspend fun pairThenConnect(candidate: Attempt, requestedPort: Int, discoveredPort: Boolean) {
        try {
            if (discoveredPort && state.value.pairingPort != requestedPort) throw SetupException(RestrictedSetupFailure.PAIRING_FAILED)
            val endpoint = endpoint(requestedPort)
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
                        networkNotAllowed = false,
                    )
                    RestrictedSetupCodeInbox.sink = { code -> submitCode(code) }
                    candidate.hasConnectService = false
                    candidate.onSinceMillis = null
                    candidate.negativeChecks = 0
                    candidate.connectPresenceDiscovery = controllerScope.launch { collectConnectPresence(candidate) }
                    candidate.networkHintCheck = controllerScope.launch { checkNetworkAllowed(candidate) }
                    candidate.procedure = null
                    true
                }
            }
            if (retrying) post(candidate, RestrictedSetupNotice.CODE_REJECTED)
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
            try {
                withDeadline(candidate, deadlines.grantAndReadbackMillis, closeGraceMillis = TRANSPORT_CLOSE_GRACE_MILLIS) { transport.grantAdvancedFeatures() }
            } catch (_: TimeoutCancellationException) {
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
            }
            val readback = verify(candidate)
            val wireless = turnWirelessDebuggingOff(candidate)
            if (readback == null) fail(candidate, RestrictedSetupFailure.TIMED_OUT, wireless = wireless)
            else if (!readback.isComplete) fail(candidate, RestrictedSetupFailure.NOT_VERIFIED, readback.restrictedMessages, readback.blockedSenders, wireless)
            else complete(candidate, readback, wireless)
        } catch (error: CancellationException) { throw error }
        catch (error: SetupException) { fail(candidate, error.failure) }
        catch (_: Exception) { fail(candidate, RestrictedSetupFailure.CONNECT_FAILED) }
    }

    private suspend fun verify(candidate: Attempt): RestrictedSetupReadback? {
        update(candidate) { it.copy(stage = RestrictedSetupStage.VERIFYING) }
        return try { withTimeout(deadlines.grantAndReadbackMillis) { accessReadback.read() } } catch (_: TimeoutCancellationException) { null } catch (error: CancellationException) { throw error } catch (_: Exception) { null }
    }

    private suspend fun turnWirelessDebuggingOff(candidate: Attempt): WirelessDebuggingOutcome {
        update(candidate) { it.copy(stage = RestrictedSetupStage.TURNING_OFF) }
        return try { withDeadline(candidate, deadlines.turnOffMillis, closeGraceMillis = TRANSPORT_CLOSE_GRACE_MILLIS) { candidate.transport!!.turnWirelessDebuggingOff() }; if (wirelessDebuggingReadback.isOn() == false) WirelessDebuggingOutcome.OFF else WirelessDebuggingOutcome.REQUESTED } catch (_: TimeoutCancellationException) { WirelessDebuggingOutcome.REQUESTED } catch (error: CancellationException) { throw error } catch (_: Exception) { WirelessDebuggingOutcome.REQUESTED }
    }

    private suspend fun endpoint(port: Int): LocalEndpoint {
        val addresses = withTimeout(deadlines.pairingMillis) { localAddresses.currentAddresses() }
        return LocalEndpoint.parse("127.0.0.1", port, addresses) ?: throw SetupException(RestrictedSetupFailure.INVALID_ENDPOINT)
    }

    private suspend fun <T> withDeadline(candidate: Attempt, timeout: Long, closeGraceMillis: Long = 0, action: suspend () -> T): T {
        // A blocking ADB read ignores coroutine cancellation; closing the transport is what releases it.
        val watchdog = watchdogScope.launch { delay(timeout + closeGraceMillis); candidate.closeTransport() }
        return try { withTimeout(timeout) { withContext(Dispatchers.IO) { action() } } } finally { watchdog.cancel() }
    }

    private fun timeout(candidate: Attempt) { candidate.closeTransport(); fail(candidate, RestrictedSetupFailure.TIMED_OUT); candidate.cancel() }
    private fun attach(candidate: Attempt, transport: RestrictedSetupTransport) { synchronized(lock) { if (active === candidate) candidate.transport = transport else transport.close() } }
    private fun complete(candidate: Attempt, readback: RestrictedSetupReadback, wireless: WirelessDebuggingOutcome) { if (terminal(candidate, RestrictedSetupStage.COMPLETE, null, readback.restrictedMessages, readback.blockedSenders, wireless)) post(candidate, RestrictedSetupNotice.DONE) }
    private fun fail(candidate: Attempt, failure: RestrictedSetupFailure, access: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN, blockedSendersAccess: RestrictedSetupAccess = RestrictedSetupAccess.UNKNOWN, wireless: WirelessDebuggingOutcome = WirelessDebuggingOutcome.NOT_REQUESTED) { if (terminal(candidate, RestrictedSetupStage.FAILED, failure, access, blockedSendersAccess, wireless)) post(candidate, RestrictedSetupNotice.FAILED) }
    private fun terminal(candidate: Attempt, stage: RestrictedSetupStage, failure: RestrictedSetupFailure?, access: RestrictedSetupAccess, blockedSendersAccess: RestrictedSetupAccess, wireless: WirelessDebuggingOutcome): Boolean {
        val terminal = synchronized(lock) {
            if (active !== candidate || candidate.epoch != epoch) return@synchronized false
            mutableState.value = mutableState.value.copy(stage = stage, failure = failure, access = access, blockedSendersAccess = blockedSendersAccess, discovery = null, pairingPort = null, wirelessDebugging = wireless, networkNotAllowed = false)
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

    private inner class Attempt(val epoch: Long, val notificationAvailable: Boolean) {
        var code: CharArray? = null
        var transport: RestrictedSetupTransport? = null
        var pairingDiscovery: Job? = null
        var connectPresenceDiscovery: Job? = null
        var networkHintCheck: Job? = null
        var connectDiscovery: Job? = null
        var procedure: Job? = null
        var totalWatchdog: Job? = null
        var addressCheck: Job? = null
        var hadPairingPort = false
        @Volatile var hasConnectService = false
        var onSinceMillis: Long? = null
        var negativeChecks = 0
        var lastNotice: RestrictedSetupNotice? = null
        var connecting = false
        private val closed = AtomicBoolean(false)
        fun cancel() { code?.fill('\u0000'); code = null; cancelJobs(); closeTransport(); synchronized(lock) { if (active === this) active = null } }
        fun cancelJobs() { pairingDiscovery?.cancel(); connectPresenceDiscovery?.cancel(); networkHintCheck?.cancel(); connectDiscovery?.cancel(); procedure?.cancel(); totalWatchdog?.cancel(); addressCheck?.cancel() }
        fun resetTransport() { closeTransport(); transport = null; closed.set(false) }
        fun closeTransport() { val value = transport ?: return; if (closed.compareAndSet(false, true)) watchdogScope.launch(Dispatchers.IO) { if (runCatching { value.close() }.isFailure) handleTeardownFailure(this@Attempt) } }
    }

    private fun handleTeardownFailure(candidate: Attempt) {
        val failed = synchronized(lock) {
            teardownFailed = true
            mutableState.value = mutableState.value.copy(retryBlocked = true)
            if (mutableState.value.stage == RestrictedSetupStage.COMPLETE || active !== candidate || candidate.epoch != epoch) false
            else {
                mutableState.value = mutableState.value.copy(stage = RestrictedSetupStage.FAILED, failure = RestrictedSetupFailure.TRANSPORT, discovery = null, pairingPort = null, networkNotAllowed = false)
                RestrictedSetupCodeInbox.sink = null
                active = null
                true
            }
        }
        if (failed) {
            candidate.cancel()
            post(candidate, RestrictedSetupNotice.FAILED)
        }
    }
}

private val RestrictedSetupReadback.isComplete: Boolean
    get() = listOf(restrictedMessages, blockedSenders).let { accesses ->
        accesses.any { it != RestrictedSetupAccess.UNSUPPORTED } &&
            accesses.all { it == RestrictedSetupAccess.ALLOWED || it == RestrictedSetupAccess.UNSUPPORTED }
    }

internal fun Pair<RestrictedMessageAccess, RestrictedMessageAccess>.toSetupReadback() =
    RestrictedSetupReadback(first.toSetupAccess(), second.toSetupAccess())

internal fun RestrictedMessageAccess.toSetupAccess(): RestrictedSetupAccess = when (this) {
    RestrictedMessageAccess.ALLOWED -> RestrictedSetupAccess.ALLOWED
    RestrictedMessageAccess.NOT_ALLOWED -> RestrictedSetupAccess.NOT_ALLOWED
    RestrictedMessageAccess.UNSUPPORTED -> RestrictedSetupAccess.UNSUPPORTED
    RestrictedMessageAccess.UNKNOWN -> RestrictedSetupAccess.UNKNOWN
}
