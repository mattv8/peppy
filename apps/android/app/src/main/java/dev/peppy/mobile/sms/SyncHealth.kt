package dev.peppy.mobile.sms

import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.ServerReachability
import dev.peppy.mobile.gateway.SyncPhase

internal fun syncStopped(status: GatewayStatus?): Boolean =
    status != null && (status.syncPhase == SyncPhase.RECOVERY_REQUIRED || status.outboxRejection != null)

internal enum class SyncSummaryState { CHECKING, ATTENTION, TOO_LARGE, IMPORTING, PROGRESS, RETRYING, OFF, HISTORY_PAUSED, HEALTHY }

internal data class SyncHealth(
    val unlocked: Boolean, val receiveAllowed: Boolean, val sendAllowed: Boolean,
    val sendPausedByAccount: Boolean, val readAllowed: Boolean, val smsSyncOn: Boolean,
    val connected: Boolean, val stopped: Boolean, val history: SmsHistoryState,
    val reachability: ServerReachability, val syncPhase: SyncPhase, val softRejected: Boolean,
    val historyTooLarge: Boolean = false,
    /** Too large from a download byte cap: the recheck is 7 days, so no "every 6 hours" copy. */
    val historyTooLargeByteCap: Boolean = false,
) {
    val problemCount: Int get() = listOf(
        !unlocked, stopped,
        smsSyncOn && !receiveAllowed,
        smsSyncOn && !sendAllowed && !sendPausedByAccount,
        smsSyncOn && !readAllowed,
    ).count { it }
    val actionNeeded: Boolean get() = problemCount > 0
    val summaryState: SyncSummaryState get() = when {
        stopped || !unlocked -> SyncSummaryState.ATTENTION
        historyTooLarge -> SyncSummaryState.TOO_LARGE
        syncPhase == SyncPhase.BOOTSTRAP || syncPhase == SyncPhase.BOOTSTRAP_DRAINING -> SyncSummaryState.IMPORTING
        !smsSyncOn -> SyncSummaryState.OFF
        stopped || actionNeeded -> SyncSummaryState.ATTENTION
        reachability == ServerReachability.UNKNOWN -> SyncSummaryState.CHECKING
        reachability == ServerReachability.UNAVAILABLE || softRejected || history == SmsHistoryState.UNAVAILABLE -> SyncSummaryState.RETRYING
        syncPhase != SyncPhase.LIVE || history == SmsHistoryState.READING || history == SmsHistoryState.PENDING -> SyncSummaryState.PROGRESS
        history == SmsHistoryState.PAUSED -> SyncSummaryState.HISTORY_PAUSED
        connected && history == SmsHistoryState.CAPTURED -> SyncSummaryState.HEALTHY
        else -> SyncSummaryState.PROGRESS
    }
}

internal fun syncHealth(status: GatewayStatus, receiveGranted: Boolean, sendGranted: Boolean, readGranted: Boolean,
                        accountReadOnly: Boolean, smsSyncOn: Boolean, history: SmsHistoryState,
                        reachability: ServerReachability) = SyncHealth(
    unlocked = status.databaseOpen && status.sharedKeysReady,
    receiveAllowed = receiveGranted, sendAllowed = sendGranted, sendPausedByAccount = accountReadOnly,
    readAllowed = readGranted, smsSyncOn = smsSyncOn,
    connected = reachability == ServerReachability.AVAILABLE && status.sharedKeysReady && status.syncPhase == SyncPhase.LIVE && status.softRejection == null && !syncStopped(status),
    stopped = syncStopped(status), history = history,
    reachability = reachability, syncPhase = status.syncPhase, softRejected = status.softRejection != null,
    historyTooLarge = status.historyTooLarge,
    historyTooLargeByteCap = status.historyTooLargeByteCap,
)
