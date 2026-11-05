package dev.peppy.mobile.googlemessages

class RunBackoff {
    private var retry = 0

    fun nextDelayMillis(): Long = DELAYS_MS[retry.coerceAtMost(DELAYS_MS.lastIndex)].also { retry++ }

    fun reset() {
        retry = 0
    }

    private companion object {
        val DELAYS_MS = longArrayOf(30_000L, 60_000L, 120_000L, 300_000L, 600_000L)
    }
}

internal enum class ServiceAction { STOP, RUN, WAIT }

internal fun nextServiceAction(status: LinkStatus, pairingInProgress: Boolean, running: Boolean): ServiceAction = when {
    pairingInProgress || status.state == "pairing" -> ServiceAction.WAIT
    status.state in TERMINAL_STATES -> ServiceAction.STOP
    status.state == "linked" && !running -> ServiceAction.RUN
    else -> ServiceAction.WAIT
}

private val TERMINAL_STATES = setOf("unlinked", "needs_relink", "signed_out")
