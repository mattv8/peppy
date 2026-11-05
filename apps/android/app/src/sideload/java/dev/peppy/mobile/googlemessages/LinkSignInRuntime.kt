package dev.peppy.mobile.googlemessages

import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

internal object LinkSignInRuntime {
    private val lock = Any()
    private val pairingExecutor: ExecutorService = Executors.newSingleThreadExecutor()
    private var lease: Lease? = null
    private var signInBlocked = false

    fun acquireLease(): Lease? = synchronized(lock) {
        if (signInBlocked || lease != null) return null
        Lease(++nextGeneration).also { lease = it }
    }

    fun ownsLease(candidate: Lease?): Boolean = synchronized(lock) { candidate != null && lease === candidate }

    fun releaseLease(candidate: Lease) = synchronized(lock) {
        if (lease === candidate) lease = null
    }

    fun beginPairing(): PairingAttempt? = LinkRuntime.tryBeginPairing()?.let(::PairingAttempt)

    fun executePairing(attempt: PairingAttempt, pair: () -> Unit): Boolean {
        return try {
            pairingExecutor.execute {
                try {
                    runCatching(pair)
                } finally {
                    finishPairing(attempt)
                }
            }
            true
        } catch (_: RuntimeException) {
            false
        }
    }

    fun finishPairing(attempt: PairingAttempt): Boolean = LinkRuntime.endPairing(attempt.token)

    fun pairingInProgress(): Boolean = LinkRuntime.pairingInProgress.get()

    fun blockSignIn() = synchronized(lock) { signInBlocked = true }

    fun isSignInBlocked(): Boolean = synchronized(lock) { signInBlocked }

    fun completeCleanup(candidate: Lease, success: Boolean): Boolean = synchronized(lock) {
        if (lease !== candidate) return false
        if (!success) signInBlocked = true
        lease = null
        true
    }

    internal fun resetForTest() = synchronized(lock) {
        lease = null
        signInBlocked = false
        LinkRuntime.endPairing()
    }

    class Lease internal constructor(val generation: Long)
    class PairingAttempt internal constructor(val token: Long)

    private var nextGeneration = 0L
}
