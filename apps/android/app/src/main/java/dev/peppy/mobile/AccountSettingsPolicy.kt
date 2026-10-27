package dev.peppy.mobile

/** Authorization checks used by the Account navigation rows. */
internal object AccountSettingsPolicy {
    fun canAddPhone(role: String, hostAllowsAddPhone: Boolean) = hostAllowsAddPhone && role == "owner"
}
