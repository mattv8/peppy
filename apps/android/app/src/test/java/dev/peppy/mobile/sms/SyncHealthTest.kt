package dev.peppy.mobile.sms

import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.ServerReachability
import dev.peppy.mobile.gateway.SoftRejection
import dev.peppy.mobile.gateway.SyncPhase
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SyncHealthTest {
    @Test
    fun availableLiveCapturedSyncIsHealthy() {
        val health = health()

        assertEquals(0, health.problemCount)
        assertEquals(SyncSummaryState.HEALTHY, health.summaryState)
    }

    @Test
    fun closedDatabaseNeedsUnlocking() {
        val health = health(databaseOpen = false)

        assertEquals(1, health.problemCount)
        assertFalse(health.unlocked)
    }

    @Test
    fun deniedReceiveOnlyMattersWhenSmsSyncIsOn() {
        assertEquals(0, health(receiveGranted = false, smsSyncOn = false).problemCount)
        assertEquals(1, health(receiveGranted = false).problemCount)
    }

    @Test
    fun accountReadOnlyPausesSendWithoutAProblem() {
        assertEquals(0, health(sendGranted = false, accountReadOnly = true).problemCount)
    }

    @Test
    fun recoveryStopsSyncAndDisconnects() {
        val health = health(syncPhase = SyncPhase.RECOVERY_REQUIRED)

        assertTrue(health.stopped)
        assertFalse(health.connected)
    }

    @Test
    fun outboxRejectionStopsSync() {
        assertTrue(health(outboxRejection = "x").stopped)
    }

    @Test
    fun softRejectionDisconnectsWithoutAddingAProblem() {
        val health = health(softRejection = SoftRejection("x", 0, 1))

        assertFalse(health.connected)
        assertEquals(0, health.problemCount)
        assertEquals(SyncSummaryState.RETRYING, health.summaryState)
    }

    @Test
    fun pausedHistoryIsNotAnActionAndShowsHistoryPaused() {
        val health = health(history = SmsHistoryState.PAUSED)

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals("HISTORY_PAUSED", health.summaryState.name)
    }

    @Test
    fun pausedHistoryKeepsMissingReceivePermissionAsAttention() {
        val health = health(history = SmsHistoryState.PAUSED, receiveGranted = false)

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryKeepsMissingSendPermissionAsAttention() {
        val health = health(history = SmsHistoryState.PAUSED, sendGranted = false)

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryKeepsMissingReadPermissionAsAttention() {
        val health = health(history = SmsHistoryState.PAUSED, readGranted = false)

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryKeepsLockedVaultAsAttention() {
        val health = health(history = SmsHistoryState.PAUSED, databaseOpen = false)

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryKeepsStoppedSyncAsAttentionWithOneProblem() {
        val health = health(history = SmsHistoryState.PAUSED, syncPhase = SyncPhase.RECOVERY_REQUIRED)

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryKeepsOutboxRejectionAsAttentionWithOneProblem() {
        val health = health(history = SmsHistoryState.PAUSED, outboxRejection = "x")

        assertEquals(1, health.problemCount)
        assertTrue(health.actionNeeded)
        assertEquals(SyncSummaryState.ATTENTION, health.summaryState)
    }

    @Test
    fun pausedHistoryWithSmsOffIsOffWithoutAProblem() {
        val health = health(history = SmsHistoryState.PAUSED, smsSyncOn = false)

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals(SyncSummaryState.OFF, health.summaryState)
    }

    @Test
    fun pausedHistoryWithUnknownReachabilityIsCheckingWithoutAProblem() {
        val health = health(history = SmsHistoryState.PAUSED, reachability = ServerReachability.UNKNOWN)

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals(SyncSummaryState.CHECKING, health.summaryState)
    }

    @Test
    fun pausedHistoryWithUnavailableServerRetriesWithoutAProblem() {
        val health = health(history = SmsHistoryState.PAUSED, reachability = ServerReachability.UNAVAILABLE)

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals(SyncSummaryState.RETRYING, health.summaryState)
    }

    @Test
    fun pausedHistoryWithSoftRejectionRetriesWithoutAProblem() {
        val health = health(history = SmsHistoryState.PAUSED, softRejection = SoftRejection("x", 0, 1))

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals(SyncSummaryState.RETRYING, health.summaryState)
    }

    @Test
    fun pausedHistoryDuringNonLiveSyncIsProgressWithoutAProblem() {
        val health = health(history = SmsHistoryState.PAUSED, syncPhase = SyncPhase.BOOTSTRAP_DRAINING)

        assertEquals(0, health.problemCount)
        assertFalse(health.actionNeeded)
        assertEquals(SyncSummaryState.PROGRESS, health.summaryState)
    }

    @Test
    fun pendingAndReadingHistoryDoNotAddProblems() {
        assertEquals(SyncSummaryState.PROGRESS, health(history = SmsHistoryState.PENDING).summaryState)
        assertEquals(SyncSummaryState.PROGRESS, health(history = SmsHistoryState.READING).summaryState)
    }

    @Test
    fun unavailableHistoryRetriesWithoutClaimingHealth() {
        assertEquals(SyncSummaryState.RETRYING, health(history = SmsHistoryState.UNAVAILABLE).summaryState)
    }

    @Test
    fun reachabilityGatesConnectionAndSummary() {
        assertFalse(health(reachability = ServerReachability.UNKNOWN).connected)
        assertEquals(SyncSummaryState.CHECKING, health(reachability = ServerReachability.UNKNOWN).summaryState)
        assertFalse(health(reachability = ServerReachability.UNAVAILABLE).connected)
        assertEquals(SyncSummaryState.RETRYING, health(reachability = ServerReachability.UNAVAILABLE).summaryState)
    }

    @Test
    fun smsOffDoesNotWarnAboutMissingPermissions() {
        val health = health(receiveGranted = false, sendGranted = false, readGranted = false, smsSyncOn = false)

        assertEquals(0, health.problemCount)
        assertEquals(SyncSummaryState.OFF, health.summaryState)
    }

    private fun health(
        databaseOpen: Boolean = true,
        receiveGranted: Boolean = true,
        sendGranted: Boolean = true,
        accountReadOnly: Boolean = false,
        readGranted: Boolean = true,
        smsSyncOn: Boolean = true,
        history: SmsHistoryState = SmsHistoryState.CAPTURED,
        syncPhase: SyncPhase = SyncPhase.LIVE,
        outboxRejection: String? = null,
        softRejection: SoftRejection? = null,
        reachability: ServerReachability = ServerReachability.AVAILABLE,
    ) = syncHealth(
        GatewayStatus(true, null, databaseOpen, true, syncPhase, outboxRejection = outboxRejection, softRejection = softRejection),
        receiveGranted,
        sendGranted,
        readGranted = readGranted,
        accountReadOnly,
        smsSyncOn,
        history,
        reachability,
    )
}
