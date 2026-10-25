package dev.peppy.mobile

import android.content.Context

/**
 * Durable host-side sync phase. A fresh database never replays live history: it bootstraps from a
 * snapshot (whose commands the core imports as historical), and only after that decision may it
 * upload or touch the carrier.
 */
enum class SyncPhase {
    /** Fresh enrollment: no live replay, upload, or carrier work until the snapshot is applied. */
    BOOTSTRAP,
    /** Bootstrap snapshot published; draining until `snapshotRemaining == 0`. */
    BOOTSTRAP_DRAINING,
    LIVE,
    /** Terminal until re-enrollment as a new device: no upload, no carrier work. */
    RECOVERY_REQUIRED,
}

enum class RecoveryReason {
    /** The server already holds records produced by this device ID (earlier install / copied credential). */
    REUSED_DEVICE_IDENTITY,
    /** The server rejected an upload as a producer idempotency conflict. */
    PRODUCER_CONFLICT,
    /**
     * The local database file disappeared after it had been initialized. Recreating it would lose
     * the attempt ledger and could re-execute live commands, so it is never recreated.
     */
    DATABASE_MISSING,
}

/**
 * A code-less upload rejection (bare `http_<status>`) that may be transient infrastructure. Uploads
 * retry on later passes until [escalates]; the envelope is never acked while soft-rejected.
 */
data class SoftRejection(val code: String, val firstSeenMs: Long, val attempts: Int) {
    /** Only a rejection that persists across retries and time becomes permanent. */
    fun escalates(nowMs: Long): Boolean {
        val age = nowMs - firstSeenMs
        return if (code in SHORT_FUSE_CODES) age >= HOUR_MS && attempts >= 3 else age >= 48 * HOUR_MS && attempts >= 10
    }

    private companion object {
        const val HOUR_MS = 3_600_000L
        val SHORT_FUSE_CODES = setOf("http_400", "http_413", "http_415", "http_422")
    }
}

class GatewayStateStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Absent state is treated as a fresh bootstrap, never as live. */
    val phase: SyncPhase
        get() = prefs.getString(P_PHASE, null)?.let { name -> SyncPhase.entries.firstOrNull { it.name == name } } ?: SyncPhase.BOOTSTRAP

    val recoveryReason: RecoveryReason?
        get() = prefs.getString(P_RECOVERY, null)?.let { name -> RecoveryReason.entries.firstOrNull { it.name == name } }

    /** Server error code of a permanently rejected outbox envelope; uploads and carrier work stop. */
    val outboxRejection: String?
        get() = prefs.getString(P_OUTBOX_REJECTION, null)

    val softRejection: SoftRejection?
        get() {
            val code = prefs.getString(P_SOFT_CODE, null) ?: return null
            return SoftRejection(code, prefs.getLong(P_SOFT_FIRST_SEEN, 0L), prefs.getInt(P_SOFT_ATTEMPTS, 0))
        }

    fun startBootstrap() = commit { clear().putString(P_PHASE, SyncPhase.BOOTSTRAP.name) }

    /** Drops state belonging to an archived enrollment before a different device is imported. */
    fun resetForNewEnrollment() = startBootstrap()

    fun advance(to: SyncPhase) = synchronized(LOCK) {
        // RECOVERY_REQUIRED can only be left by a new enrollment (startBootstrap on a new database).
        if (phase != SyncPhase.RECOVERY_REQUIRED) commit { putString(P_PHASE, to.name) }
    }

    fun requireRecovery(reason: RecoveryReason) = commit {
        putString(P_PHASE, SyncPhase.RECOVERY_REQUIRED.name).putString(P_RECOVERY, reason.name)
    }

    /** Permanent rejection; it supersedes any soft rejection. */
    fun rejectOutbox(code: String) = commit { putString(P_OUTBOX_REJECTION, code.take(64)).clearSoftFields() }

    /** Counts one failed upload attempt; first-seen restarts on a new code or when the clock stepped back. */
    fun recordSoftRejection(code: String, nowMs: Long): SoftRejection = synchronized(LOCK) {
        val previous = softRejection?.takeIf { it.code == code }
        val record = SoftRejection(code, previous?.firstSeenMs?.takeIf { it <= nowMs } ?: nowMs, (previous?.attempts ?: 0) + 1)
        commit { putString(P_SOFT_CODE, record.code).putLong(P_SOFT_FIRST_SEEN, record.firstSeenMs).putInt(P_SOFT_ATTEMPTS, record.attempts) }
        record
    }

    fun clearSoftRejection() = commit { clearSoftFields() }

    /**
     * Upgrade repair: an earlier build made a code-less rejection permanent, which blocked uploads
     * until re-pairing. It becomes a soft rejection instead. Runs once; RECOVERY_REQUIRED is untouched.
     */
    fun migrateRejectionPolicy(nowMs: Long) {
        synchronized(LOCK) {
            if (phase == SyncPhase.RECOVERY_REQUIRED || prefs.getBoolean(P_REJECTION_POLICY_V2, false)) return
            val stored = outboxRejection
            commit {
                if (stored != null && CODELESS_HTTP_CODE.matches(stored)) {
                    remove(P_OUTBOX_REJECTION).putString(P_SOFT_CODE, stored).putLong(P_SOFT_FIRST_SEEN, nowMs).putInt(P_SOFT_ATTEMPTS, 0)
                }
                putBoolean(P_REJECTION_POLICY_V2, true)
            }
        }
    }

    private fun android.content.SharedPreferences.Editor.clearSoftFields() =
        remove(P_SOFT_CODE).remove(P_SOFT_FIRST_SEEN).remove(P_SOFT_ATTEMPTS)

    private inline fun commit(edit: android.content.SharedPreferences.Editor.() -> android.content.SharedPreferences.Editor) {
        synchronized(LOCK) { check(prefs.edit().edit().commit()) { "gateway state not persisted" } }
    }

    private companion object {
        const val PREFS = "peppy-gateway-state"
        const val P_PHASE = "phase.v1"
        const val P_RECOVERY = "recovery.v1"
        const val P_OUTBOX_REJECTION = "outbox-rejection.v1"
        const val P_SOFT_CODE = "soft-rejection-code.v1"
        const val P_SOFT_FIRST_SEEN = "soft-rejection-first-seen.v1"
        const val P_SOFT_ATTEMPTS = "soft-rejection-attempts.v1"
        const val P_REJECTION_POLICY_V2 = "rejection-policy.v2"
        val CODELESS_HTTP_CODE = Regex("http_4\\d\\d")
        val LOCK = Any()
    }
}
