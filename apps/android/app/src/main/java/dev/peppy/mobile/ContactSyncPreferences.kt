package dev.peppy.mobile

import android.content.Context

/**
 * Local consent only. Policy, default account, books, scan checkpoints and edit state live in the
 * Rust core; this file never mirrors them.
 */
internal enum class ContactSyncIntent { UNSET, ON, STOPPED }

internal class ContactSyncPreferences(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(NAME, Context.MODE_PRIVATE)

    /** The legacy key remains the durable local intent: absent is unset, false is stopped. */
    var enabled: Boolean
        get() = intent == ContactSyncIntent.ON
        set(value) { prefs.edit().putBoolean(ENABLED, value).apply() }

    val intent: ContactSyncIntent
        get() = when {
            retirePending -> ContactSyncIntent.STOPPED
            !prefs.contains(ENABLED) -> ContactSyncIntent.UNSET
            prefs.getBoolean(ENABLED, false) -> ContactSyncIntent.ON
            else -> ContactSyncIntent.STOPPED
        }

    /**
     * Foreground permission reconciliation only; workers never promote. A pending start from an
     * explicit user start is consumed once by a read grant; otherwise only an unset intent promotes.
     * A retirement pending from a locked stop never resumes, and an explicit stop stays stopped.
     */
    fun promoteGrantedUnset(readGranted: Boolean): Boolean {
        if (!readGranted || retirePending) return false
        if (startAfterReadGrant) {
            enabled = true
            startAfterReadGrant = false
            return true
        }
        if (intent != ContactSyncIntent.UNSET) return false
        enabled = true
        return true
    }

    var readPermissionRequested: Boolean
        get() = prefs.getBoolean(READ_PERMISSION_REQUESTED, false)
        set(value) { prefs.edit().putBoolean(READ_PERMISSION_REQUESTED, value).apply() }

    var startAfterReadGrant: Boolean
        get() = prefs.getBoolean(START_AFTER_READ_GRANT, false)
        set(value) { prefs.edit().putBoolean(START_AFTER_READ_GRANT, value).apply() }

    var readAccessGranted: Boolean
        get() = prefs.getBoolean(READ_ACCESS_GRANTED, false)
        set(value) { prefs.edit().putBoolean(READ_ACCESS_GRANTED, value).apply() }

    /**
     * Set when the user stopped and retired sync while the vault was locked; the next unlocked
     * pass publishes the retirement. Committed synchronously so a kill cannot lose the request.
     */
    var retirePending: Boolean
        get() = prefs.getBoolean(RETIRE_PENDING, false)
        set(value) { check(prefs.edit().putBoolean(RETIRE_PENDING, value).commit()) }

    private companion object {
        const val NAME = "peppy-contact-sync"
        const val ENABLED = "enabled"
        const val RETIRE_PENDING = "retire-pending"
        const val READ_PERMISSION_REQUESTED = "read-permission-requested"
        const val START_AFTER_READ_GRANT = "start-after-read-grant"
        const val READ_ACCESS_GRANTED = "read-access-granted"
    }
}

data class ContactAccount(val name: String, val type: String) {
    val label: String get() = "$type · $name"
}
