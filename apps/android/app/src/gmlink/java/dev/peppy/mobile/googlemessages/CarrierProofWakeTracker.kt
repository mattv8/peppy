package dev.peppy.mobile.googlemessages

/** Process-lifetime bound for carrier-only proof handoffs; normal gateway cadence retries later. */
internal class CarrierProofWakeTracker(private val maximumWakes: Int = MAXIMUM_WAKES) {
    private val consecutive = LinkedHashMap<String, Int>()

    /** Requires the full pending ledger, never a filtered or bounded native candidate page. */
    fun retainPendingCommands(commandIds: Set<String>) {
        consecutive.keys.retainAll(commandIds)
    }

    fun record(commandId: String, outcome: RcsNativeSendOutcome): CarrierProofWakeAction {
        if (outcome != RcsNativeSendOutcome.CARRIER_ONLY) {
            consecutive.remove(commandId)
            return CarrierProofWakeAction.NONE
        }
        if (commandId !in consecutive && consecutive.size >= MAX_TRACKED_COMMANDS) {
            // Never evict an existing fuse: an untracked command falls back to normal cadence.
            return CarrierProofWakeAction.PROOF_WINDOW_MISSED
        }
        val count = (consecutive[commandId] ?: 0) + 1
        consecutive[commandId] = count
        return if (count <= maximumWakes) CarrierProofWakeAction.WAKE else CarrierProofWakeAction.PROOF_WINDOW_MISSED
    }

    private companion object {
        const val MAXIMUM_WAKES = 3
        const val MAX_TRACKED_COMMANDS = 128
    }
}

internal enum class CarrierProofWakeAction { NONE, WAKE, PROOF_WINDOW_MISSED }
