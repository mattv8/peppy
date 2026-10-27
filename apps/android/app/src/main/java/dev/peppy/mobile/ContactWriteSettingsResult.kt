package dev.peppy.mobile

/** Next write-consent state after an OS settings return, plus the remote-edit mode to apply, if any. */
internal data class ContactWriteSettingsResult(val state: ConsentGatedToggle, val modeToApply: String?)

/**
 * Consumes a write-access return once. Only a pending intent that is awaiting access can apply,
 * and only with a chosen mode; any other outcome lands in IDLE, except an already-running or
 * unrelated phase, which is left untouched so a repeated return cannot apply twice.
 */
internal fun contactWriteSettingsResult(
    current: ConsentGatedToggle,
    selectedMode: String?,
    granted: Boolean,
    pending: Boolean,
): ContactWriteSettingsResult {
    if (current.phase != ConsentPhase.AWAITING_ACCESS) return ContactWriteSettingsResult(current, null)
    if (!pending) return ContactWriteSettingsResult(ConsentGatedToggle(), null)
    val transition = current.accessResult(granted)
    if (transition.effect != ConsentEffect.APPLY) return ContactWriteSettingsResult(transition.state, null)
    return if (selectedMode == null) ContactWriteSettingsResult(ConsentGatedToggle(), null)
    else ContactWriteSettingsResult(transition.state, selectedMode)
}
