package dev.peppy.mobile.gateway

import android.content.Context
import android.content.SharedPreferences
import androidx.core.content.pm.PackageInfoCompat
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow

/** Bootstrap is the only phase that shows setup progress (import card, its persisted counts). */
internal fun SyncPhase.isBootstrapPhase(): Boolean = this == SyncPhase.BOOTSTRAP || this == SyncPhase.BOOTSTRAP_DRAINING

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
 * Which step refused a snapshot as too large. A begin refusal (record ceiling) downloaded nothing and rechecks
 * sooner; a byte-cap refusal from a page append follows a large download, so it waits longer before retrying.
 */
enum class HistoryTooLargeKind(internal val recheckMs: Long) {
    SNAPSHOT_BEGIN(6 * 60 * 60 * 1000L),
    DOWNLOAD_BYTE_CAP(7 * 24 * 60 * 60 * 1000L),
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
    /** Compat lookup: `PackageInfo.longVersionCode` is API 28 and minSdk is 26. */
    private val versionCode by lazy { PackageInfoCompat.getLongVersionCode(context.packageManager.getPackageInfo(context.packageName, 0)) }

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
        if (phase != SyncPhase.RECOVERY_REQUIRED) commit {
            putString(P_PHASE, to.name).also { if (to == SyncPhase.LIVE) it.clearSetupFields().clearHistoryTooLargeFields() }
        }
    }

    /**
     * Emits once right after the listener is registered, then after every change to the gateway state. The first
     * signal covers any write made before registration, so a reader that read its status before registering
     * still reloads after the last write. The listener is held strongly by this flow's scope: SharedPreferences
     * keeps its listeners weakly. [beforeRegistration] is a test seam that runs just before registration.
     */
    fun changes(beforeRegistration: suspend () -> Unit = {}): Flow<Unit> = callbackFlow {
        beforeRegistration()
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, _ -> trySend(Unit) }
        prefs.registerOnSharedPreferenceChangeListener(listener)
        trySend(Unit)
        awaitClose { prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }

    /** Setup progress is a bootstrap-only concept: stale fields left by an older build are dropped once LIVE or recovered. */
    fun clearStaleSetupProgress() = synchronized(LOCK) {
        if (!phase.isBootstrapPhase()) commit { clearSetupFields() }
    }

    fun requireRecovery(reason: RecoveryReason) = commit {
        putString(P_PHASE, SyncPhase.RECOVERY_REQUIRED.name).putString(P_RECOVERY, reason.name).clearSetupFields()
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

    data class SetupProgressState(val phase: SetupPhase, val staged: Long = 0, val expected: Long = 0, val remaining: Long = 0)
    enum class SetupPhase { CHECKING, DOWNLOADING, ORGANIZING }

    val setupProgress: SetupProgressState?
        get() = if (!phase.isBootstrapPhase()) null else prefs.getString(P_SETUP_PHASE, null)?.let { value ->
            SetupPhase.entries.firstOrNull { it.name == value }?.let { phase ->
                SetupProgressState(phase, prefs.getLong(P_SETUP_STAGED, 0), prefs.getLong(P_SETUP_EXPECTED, 0), prefs.getLong(P_SETUP_REMAINING, 0))
            }
        }

    val historyTooLargeCheckedMs: Long?
        get() = prefs.getLong(P_HISTORY_TOO_LARGE_CHECKED, 0).takeIf { it > 0 }

    val hasHistoryTooLarge: Boolean get() = historyTooLargeCheckedMs != null

    /** The kind of the active marker. A marker written before the kind existed is [HistoryTooLargeKind.SNAPSHOT_BEGIN]. */
    val historyTooLargeKind: HistoryTooLargeKind?
        get() = if (!hasHistoryTooLarge) null else HistoryTooLargeKind.entries
            .firstOrNull { it.name == prefs.getString(P_HISTORY_TOO_LARGE_KIND, null) } ?: HistoryTooLargeKind.SNAPSHOT_BEGIN

    /**
     * Whether a new snapshot begin may be attempted now. Side-effect free: the marker is cleared only by a
     * successful begin ([clearHistoryTooLarge]). Allowed with no marker, after a build version change, or
     * once the active kind's recheck window has elapsed.
     */
    fun snapshotAttemptAllowed(nowMs: Long): Boolean {
        val checkedMs = historyTooLargeCheckedMs ?: return true
        if (prefs.getLong(P_HISTORY_TOO_LARGE_VERSION, -1) != versionCode) return true
        val recheckMs = (historyTooLargeKind ?: return true).recheckMs
        return nowMs - checkedMs >= recheckMs
    }

    /** A too-large state replaces any setup count: the card would otherwise show a frozen import beside the warning. */
    fun recordHistoryTooLarge(nowMs: Long, kind: HistoryTooLargeKind = HistoryTooLargeKind.SNAPSHOT_BEGIN) = commit {
        putLong(P_HISTORY_TOO_LARGE_CHECKED, nowMs)
            .putLong(P_HISTORY_TOO_LARGE_VERSION, versionCode)
            .putString(P_HISTORY_TOO_LARGE_KIND, kind.name)
            .clearSetupFields()
    }

    fun clearHistoryTooLarge() = commit { clearHistoryTooLargeFields() }

    fun setChecking() = writeSetupProgress(SetupProgressState(SetupPhase.CHECKING))
    fun setDownloading(staged: ULong, expected: ULong) = writeSetupProgress(SetupProgressState(SetupPhase.DOWNLOADING, staged.toLong(), expected.toLong()))
    fun setOrganizing(remaining: ULong) = writeSetupProgress(SetupProgressState(SetupPhase.ORGANIZING, remaining = remaining.toLong()))

    /** Persists only while bootstrapping: a LIVE resync or contact repair must never leave an import card behind. */
    fun writeSetupProgress(progress: SetupProgressState) = synchronized(LOCK) {
        if (phase.isBootstrapPhase()) commit {
            putString(P_SETUP_PHASE, progress.phase.name)
                .putLong(P_SETUP_STAGED, progress.staged)
                .putLong(P_SETUP_EXPECTED, progress.expected)
                .putLong(P_SETUP_REMAINING, progress.remaining)
        }
    }

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

    private fun android.content.SharedPreferences.Editor.clearSetupFields() =
        remove(P_SETUP_PHASE).remove(P_SETUP_STAGED).remove(P_SETUP_EXPECTED).remove(P_SETUP_REMAINING)

    private fun android.content.SharedPreferences.Editor.clearHistoryTooLargeFields() =
        remove(P_HISTORY_TOO_LARGE_CHECKED).remove(P_HISTORY_TOO_LARGE_VERSION).remove(P_HISTORY_TOO_LARGE_KIND)

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
        const val P_HISTORY_TOO_LARGE_CHECKED = "history-too-large-checked.v1"
        const val P_HISTORY_TOO_LARGE_VERSION = "history-too-large-version.v1"
        const val P_HISTORY_TOO_LARGE_KIND = "history-too-large-kind.v1"
        const val P_SETUP_PHASE = "setup-phase.v1"
        const val P_SETUP_STAGED = "setup-staged.v1"
        const val P_SETUP_EXPECTED = "setup-expected.v1"
        const val P_SETUP_REMAINING = "setup-remaining.v1"
        val CODELESS_HTTP_CODE = Regex("http_4\\d\\d")
        val LOCK = Any()
    }
}
