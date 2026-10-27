package dev.peppy.mobile

/**
 * Lifecycle phases for a capability toggle that requires explicit consent before
 * requesting OS access or writing policy. The state is pure and immutable; callers
 * hold it in their own state holder and perform the returned effect.
 */
internal enum class ConsentPhase { IDLE, DISCLOSURE, AWAITING_ACCESS, APPLYING }

/** Side effect the caller must perform after a transition. */
internal enum class ConsentEffect { NONE, REQUEST_ACCESS, APPLY }

internal data class ConsentTransition(val state: ConsentGatedToggle, val effect: ConsentEffect)

internal data class ConsentGatedToggle(val phase: ConsentPhase = ConsentPhase.IDLE) {
    /** Opens the disclosure only from IDLE; any other phase is unchanged. */
    fun requestEnable(): ConsentGatedToggle =
        if (phase == ConsentPhase.IDLE) copy(phase = ConsentPhase.DISCLOSURE) else this

    /** Accepts the disclosure once. Stale or duplicate confirmations are ignored. */
    fun agree(hasAccess: Boolean): ConsentTransition = when {
        phase != ConsentPhase.DISCLOSURE -> ConsentTransition(this, ConsentEffect.NONE)
        hasAccess -> ConsentTransition(copy(phase = ConsentPhase.APPLYING), ConsentEffect.APPLY)
        else -> ConsentTransition(copy(phase = ConsentPhase.AWAITING_ACCESS), ConsentEffect.REQUEST_ACCESS)
    }

    /** Consumes the pending intent on the first access result, granted or denied. */
    fun accessResult(granted: Boolean): ConsentTransition = when {
        phase != ConsentPhase.AWAITING_ACCESS -> ConsentTransition(this, ConsentEffect.NONE)
        granted -> ConsentTransition(copy(phase = ConsentPhase.APPLYING), ConsentEffect.APPLY)
        else -> ConsentTransition(copy(phase = ConsentPhase.IDLE), ConsentEffect.NONE)
    }

    /** Drops any pending intent; used on dismissal, tab exit, disposal, and after apply. */
    fun cancel(): ConsentGatedToggle = ConsentGatedToggle()
}
