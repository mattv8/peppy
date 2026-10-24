package dev.peppy.mobile

import android.content.Context

internal class DiagnosticsPreferences(context: Context) {
    private val preferences = context.getSharedPreferences("diagnostics", Context.MODE_PRIVATE)

    fun enabled(): Boolean = preferences.getBoolean(ENABLED, false)

    fun setEnabled(enabled: Boolean) {
        preferences.edit().putBoolean(ENABLED, enabled).apply()
    }

    private companion object { const val ENABLED = "enabled" }
}
