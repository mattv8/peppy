package dev.peppy.mobile.googlemessages

import android.content.Context
import uniffi.peppy_mobile_bindings.NativeLiveTypingException

/** Owns the device-local sharing choice independently of each live-core incarnation. */
internal class TypingSharingPreferences(context: Context) {
    private val preferences = context.applicationContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    fun enabled(): Boolean = preferences.getBoolean(SHARING, true)

    fun update(enabled: Boolean, apply: ((Boolean) -> Unit)?): Boolean {
        apply ?: return false
        return try {
            apply(enabled)
            persist(enabled)
        } catch (_: NativeLiveTypingException.Disabled) {
            persist(enabled)
        } catch (_: Exception) {
            false
        }
    }

    fun reapplyAfterEnable(apply: (Boolean) -> Unit): Boolean = try {
        apply(enabled())
        true
    } catch (_: Exception) {
        false
    }

    private fun persist(enabled: Boolean): Boolean = preferences.edit().putBoolean(SHARING, enabled).commit()

    private companion object {
        const val PREFERENCES = "google-messages-link"
        const val SHARING = "typing-sharing"
    }
}
