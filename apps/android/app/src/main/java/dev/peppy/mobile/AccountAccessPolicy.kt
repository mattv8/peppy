package dev.peppy.mobile

import uniffi.peppy_mobile_bindings.NativeAccountNoticeKind
import uniffi.peppy_mobile_bindings.NativeNoticeSeverity
import uniffi.peppy_mobile_bindings.isAccountPolicyDenial as isAccountPolicyDenialBinding

internal const val ACCOUNT_SNOOZE_MS = 24L * 60 * 60 * 1000
internal const val ACCOUNT_FORCED_REFRESH_DEBOUNCE_MS = 15L * 60 * 1000
internal const val ACCOUNT_GATEWAY_REFRESH_MS = 60L * 60 * 1000

/** The server's account notice, persisted so the last good answer survives restarts. */
internal data class CachedAccountNotice(
    val kind: NativeAccountNoticeKind,
    val severity: NativeNoticeSeverity,
    val deadline: Long?,
    val manageUrl: String?,
    val dismissible: Boolean,
    val snoozeKey: String,
) {
    /** The Rust decision maps every read-only state to the error severity, so this is authoritative. */
    val readOnly: Boolean get() = severity == NativeNoticeSeverity.ERROR
}

/** Last successful server answer for one enrolled binding. A null notice is a successful "no notice". */
internal data class CachedAccountAccess(val bindingKey: String, val notice: CachedAccountNotice?, val fetchedAtMs: Long)

/** What the single system ribbon slot shows; null means the slot is empty. */
internal sealed interface RibbonSlot {
    data object ServerOffline : RibbonSlot
    data class Account(val notice: CachedAccountNotice) : RibbonSlot
}

/**
 * A cached notice that no longer applies. An `ending_scheduled` deadline in the past means the
 * subscription already ended, so the stale cache must not show a ribbon or a notification.
 */
internal fun CachedAccountNotice.isStale(nowMs: Long): Boolean =
    kind == NativeAccountNoticeKind.ENDING_SCHEDULED && deadline != null && deadline * 1000 <= nowMs

/** Priority: server offline, then account error (read-only), then account warning. One slot only. */
internal fun resolveRibbonSlot(serverOffline: Boolean, notice: CachedAccountNotice?, snoozedKeys: Set<String>, nowMs: Long): RibbonSlot? = when {
    serverOffline -> RibbonSlot.ServerOffline
    notice == null || notice.isStale(nowMs) -> null
    notice.dismissible && notice.snoozeKey in snoozedKeys -> null
    else -> RibbonSlot.Account(notice)
}

/** Snoozes last exactly [ACCOUNT_SNOOZE_MS]; expired entries are dropped. Only dismissible notices may snooze. */
internal fun snoozeUntil(snoozes: Map<String, Long>, notice: CachedAccountNotice, nowMs: Long): Map<String, Long> {
    val live = snoozes.filterValues { it > nowMs }
    return if (notice.dismissible) live + (notice.snoozeKey to nowMs + ACCOUNT_SNOOZE_MS) else live
}

internal fun activeSnoozeKeys(snoozes: Map<String, Long>, nowMs: Long): Set<String> =
    snoozes.filterValues { it > nowMs }.keys

/** A notification episode runs from entering read-only until access returns to read-write. */
internal data class AccountEpisode(val active: Boolean = false, val notifiedKind: NativeAccountNoticeKind? = null)

internal sealed interface AccountNotificationAction {
    data object None : AccountNotificationAction
    data object Clear : AccountNotificationAction
    data class Post(val kind: NativeAccountNoticeKind) : AccountNotificationAction
    /** Same episode, different reason: update in place without alerting. */
    data class Update(val kind: NativeAccountNoticeKind) : AccountNotificationAction
}

/**
 * Pure decision for the one account-status notification. Call it only for definitive server
 * answers; a failed fetch never clears or reposts. A latest notice that is not an active
 * read-only state (a warning, account deletion, or nothing) cancels an active episode.
 */
internal fun accountNotificationStep(episode: AccountEpisode, notice: CachedAccountNotice?): Pair<AccountEpisode, AccountNotificationAction> {
    val readOnlyNotice = notice?.takeIf { it.readOnly && it.kind != NativeAccountNoticeKind.READ_ONLY_ACCOUNT_DELETING }
    if (readOnlyNotice == null) {
        return if (episode.active) AccountEpisode() to AccountNotificationAction.Clear else episode to AccountNotificationAction.None
    }
    val kind = readOnlyNotice.kind
    return when {
        !episode.active -> AccountEpisode(true, kind) to AccountNotificationAction.Post(kind)
        episode.notifiedKind != kind -> AccountEpisode(true, kind) to AccountNotificationAction.Update(kind)
        else -> episode to AccountNotificationAction.None
    }
}

/** Gateway passes refresh the account answer at most once an hour. */
internal fun gatewayRefreshDue(lastAttemptMs: Long, nowMs: Long): Boolean = nowMs - lastAttemptMs >= ACCOUNT_GATEWAY_REFRESH_MS

/** A policy denial forces a refresh only when the cache is not already read-only, at most once per 15 minutes. */
internal fun policyDenialRefreshDue(cached: CachedAccountNotice?, lastForcedMs: Long, nowMs: Long): Boolean =
    cached?.readOnly != true && nowMs - lastForcedMs >= ACCOUNT_FORCED_REFRESH_DEBOUNCE_MS

/** Coded `operation_not_permitted` 403: a policy pause, never an auth failure or an outbox rejection. */
internal fun HttpResult.isAccountPolicyDenial(): Boolean = isAccountPolicyDenialBinding(code.toUShort(), body.orEmpty())
