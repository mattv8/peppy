package dev.peppy.mobile

import android.content.Context

/**
 * Local consent only. Policy, default account, books, scan checkpoints and edit state live in the
 * Rust core; this file never mirrors them.
 */
internal enum class ContactSyncIntent { UNSET, ON, STOPPED }

internal class ContactSyncPreferences(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(NAME, Context.MODE_PRIVATE)

    init {
        // A former one-shot enable request must never survive into the consent-gated UI.
        if (prefs.contains(LEGACY_START_AFTER_READ_GRANT)) {
            prefs.edit().remove(LEGACY_START_AFTER_READ_GRANT).apply()
        }
    }

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

    var readPermissionRequested: Boolean
        get() = prefs.getBoolean(READ_PERMISSION_REQUESTED, false)
        set(value) { prefs.edit().putBoolean(READ_PERMISSION_REQUESTED, value).apply() }

    var writePermissionRequested: Boolean
        get() = prefs.getBoolean(WRITE_PERMISSION_REQUESTED, false)
        set(value) { prefs.edit().putBoolean(WRITE_PERMISSION_REQUESTED, value).apply() }

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
        const val WRITE_PERMISSION_REQUESTED = "write-permission-requested"
        const val READ_ACCESS_GRANTED = "read-access-granted"
        const val LEGACY_START_AFTER_READ_GRANT = "start-after-read-grant"
    }
}

data class ContactAccount(val name: String, val type: String) {
    val label: String get() = "$type · $name"
}
