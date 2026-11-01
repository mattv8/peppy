package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.peppy_mobile_bindings.NativeAccountNoticeKind
import uniffi.peppy_mobile_bindings.NativeNoticeSeverity

class AccountAccessPolicyTest {
    private val now = 1_000_000_000_000L

    private fun notice(
        kind: NativeAccountNoticeKind = NativeAccountNoticeKind.READ_ONLY_PAST_DUE,
        severity: NativeNoticeSeverity = NativeNoticeSeverity.ERROR,
        dismissible: Boolean = false,
        snoozeKey: String = "${kind.name}:none",
        deadline: Long? = null,
    ) = CachedAccountNotice(kind, severity, deadline = deadline, manageUrl = null, dismissible = dismissible, snoozeKey = snoozeKey)

    private val warning = notice(NativeAccountNoticeKind.PAYMENT_FAILED_GRACE, NativeNoticeSeverity.WARNING, dismissible = true, snoozeKey = "payment_failed_grace:1")

    @Test fun serverOfflineAlwaysPreemptsAccountNotices() {
        assertEquals(RibbonSlot.ServerOffline, resolveRibbonSlot(serverOffline = true, notice = notice(), snoozedKeys = emptySet(), nowMs = now))
        assertEquals(RibbonSlot.ServerOffline, resolveRibbonSlot(serverOffline = true, notice = warning, snoozedKeys = emptySet(), nowMs = now))
    }

    @Test fun readOnlyErrorBeatsWarningAndReturnsWhenReachable() {
        assertEquals(RibbonSlot.Account(notice()), resolveRibbonSlot(false, notice(), emptySet(), now))
        assertEquals(RibbonSlot.Account(warning), resolveRibbonSlot(false, warning, emptySet(), now))
        assertNull(resolveRibbonSlot(false, null, emptySet(), now))
    }

    @Test fun snoozedWarningHidesButReadOnlyNeverHides() {
        assertNull(resolveRibbonSlot(false, warning, setOf(warning.snoozeKey), now))
        val readOnly = notice(dismissible = false, snoozeKey = "read_only:x")
        assertEquals(RibbonSlot.Account(readOnly), resolveRibbonSlot(false, readOnly, setOf(readOnly.snoozeKey), now))
    }

    @Test fun pastEndingScheduledDeadlineHidesTheRibbonAndNotification() {
        val ending = notice(NativeAccountNoticeKind.ENDING_SCHEDULED, NativeNoticeSeverity.WARNING, dismissible = true, deadline = now / 1000)
        assertTrue(ending.isStale(now))
        assertNull(resolveRibbonSlot(false, ending, emptySet(), now))
        assertEquals(AccountNotificationAction.None, accountNotificationStep(AccountEpisode(), ending.takeUnless { it.isStale(now) }).second)
        val future = ending.copy(deadline = now / 1000 + 60)
        assertFalse(future.isStale(now))
        assertEquals(RibbonSlot.Account(future), resolveRibbonSlot(false, future, emptySet(), now))
    }

    @Test fun snoozeLastsExactlyTwentyFourHoursAndOnlyForDismissibleNotices() {
        val snoozed = snoozeUntil(emptyMap(), warning, now)
        assertEquals(now + ACCOUNT_SNOOZE_MS, snoozed[warning.snoozeKey])
        assertEquals(setOf(warning.snoozeKey), activeSnoozeKeys(snoozed, now + ACCOUNT_SNOOZE_MS - 1))
        assertTrue(activeSnoozeKeys(snoozed, now + ACCOUNT_SNOOZE_MS).isEmpty())
        assertEquals(emptyMap<String, Long>(), snoozeUntil(emptyMap(), notice(), now))
    }

    @Test fun snoozeDropsExpiredEntries() {
        val old = mapOf("gone" to now - 1)
        assertEquals(setOf("payment_failed_grace:1"), snoozeUntil(old, warning, now).keys)
    }

    @Test fun firstReadOnlyEntersEpisodeAndPostsOnce() {
        val (episode, action) = accountNotificationStep(AccountEpisode(), notice())
        assertEquals(AccountNotificationAction.Post(NativeAccountNoticeKind.READ_ONLY_PAST_DUE), action)
        assertTrue(episode.active)
        val (again, repeat) = accountNotificationStep(episode, notice())
        assertEquals(AccountNotificationAction.None, repeat)
        assertEquals(episode, again)
    }

    @Test fun reasonChangeInsideEpisodeUpdatesWithoutNewEpisode() {
        val (active, _) = accountNotificationStep(AccountEpisode(), notice())
        val canceled = notice(NativeAccountNoticeKind.READ_ONLY_CANCELED)
        val (next, action) = accountNotificationStep(active, canceled)
        assertEquals(AccountNotificationAction.Update(NativeAccountNoticeKind.READ_ONLY_CANCELED), action)
        assertEquals(AccountEpisode(true, NativeAccountNoticeKind.READ_ONLY_CANCELED), next)
    }

    @Test fun readWriteClearsOnlyAnActiveEpisode() {
        val (active, _) = accountNotificationStep(AccountEpisode(), notice())
        val (cleared, action) = accountNotificationStep(active, null)
        assertEquals(AccountNotificationAction.Clear, action)
        assertFalse(cleared.active)
        assertEquals(AccountNotificationAction.None, accountNotificationStep(cleared, null).second)
    }

    @Test fun warningAfterReadOnlyCancelsTheActiveEpisode() {
        val (active, _) = accountNotificationStep(AccountEpisode(), notice())
        val (cleared, action) = accountNotificationStep(active, warning)
        assertEquals(AccountNotificationAction.Clear, action)
        assertEquals(AccountEpisode(), cleared)
        // A later read-only answer starts a new episode and notifies again.
        assertEquals(
            AccountNotificationAction.Post(NativeAccountNoticeKind.READ_ONLY_PAST_DUE),
            accountNotificationStep(cleared, notice()).second,
        )
    }

    @Test fun accountDeletingCancelsAnActiveEpisodeButNeverStartsOne() {
        val deleting = notice(NativeAccountNoticeKind.READ_ONLY_ACCOUNT_DELETING)
        assertEquals(AccountNotificationAction.None, accountNotificationStep(AccountEpisode(), deleting).second)
        val (active, _) = accountNotificationStep(AccountEpisode(), notice())
        val (cleared, action) = accountNotificationStep(active, deleting)
        assertEquals(AccountNotificationAction.Clear, action)
        assertEquals(AccountEpisode(), cleared)
    }

    @Test fun warningsNeverNotify() {
        assertEquals(AccountNotificationAction.None, accountNotificationStep(AccountEpisode(), warning).second)
    }

    @Test fun gatewayRefreshIsHourlyAndPolicyRefreshIsDebouncedFifteenMinutes() {
        assertFalse(gatewayRefreshDue(now, now + ACCOUNT_GATEWAY_REFRESH_MS - 1))
        assertTrue(gatewayRefreshDue(now, now + ACCOUNT_GATEWAY_REFRESH_MS))
        assertFalse(policyDenialRefreshDue(null, now, now + ACCOUNT_FORCED_REFRESH_DEBOUNCE_MS - 1))
        assertTrue(policyDenialRefreshDue(null, now, now + ACCOUNT_FORCED_REFRESH_DEBOUNCE_MS))
    }

    @Test fun policyDenialSkipsRefreshWhenAlreadyReadOnly() {
        val cached = notice()
        assertFalse(policyDenialRefreshDue(cached, 0L, now))
        assertTrue(policyDenialRefreshDue(warning, 0L, now))
    }
}
