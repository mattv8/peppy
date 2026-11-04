package dev.peppy.mobile.restrictedsetup

import android.content.Context

internal class PrefsCleanupMarker(context: Context) : RestrictedSetupCleanupMarker {
    private val preferences = context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)

    override fun needsCleanup(): Boolean = preferences.getBoolean(CLEANUP_NEEDED, false)

    override fun markCleanupNeeded(): Boolean = preferences.edit().putBoolean(CLEANUP_NEEDED, true).commit()

    override fun acknowledgeCleanup(): Boolean = preferences.edit().putBoolean(CLEANUP_NEEDED, false).commit()

    private companion object {
        const val PREFERENCES_NAME = "restricted_setup"
        const val CLEANUP_NEEDED = "cleanup_needed"
    }
}
