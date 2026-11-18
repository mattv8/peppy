package dev.peppy.mobile.gateway

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayStateStoreTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val state get() = GatewayStateStore(context)
    private val SIX_HOURS_MS = 6 * 60 * 60 * 1000L
    private val SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000L
    private val PREFS_NAME = "peppy-gateway-state"

    @Test
    fun softRejectionKeepsFirstSeenAcrossAttemptsAndRestartsOnCodeChangeOrClockStep() {
        assertEquals(SoftRejection("http_403", 5_000L, 1), state.recordSoftRejection("http_403", 5_000L))
        assertEquals(SoftRejection("http_403", 5_000L, 2), state.recordSoftRejection("http_403", 6_000L))
        assertEquals("clock stepped back", SoftRejection("http_403", 4_000L, 3), state.recordSoftRejection("http_403", 4_000L))
        assertEquals(SoftRejection("http_422", 7_000L, 1), state.recordSoftRejection("http_422", 7_000L))
    }

    @Test
    fun clearSoftRejectionRemovesRecord() {
        state.recordSoftRejection("http_403", 1_000L)
        state.clearSoftRejection()
        assertNull(state.softRejection)
    }

    @Test
    fun legacyCodeLessPermanentRejectionBecomesSoftExactlyOnce() {
        state.advance(SyncPhase.LIVE)
        state.rejectOutbox("http_403")

        state.migrateRejectionPolicy(nowMs = 1_000L)

        assertNull(state.outboxRejection)
        assertEquals(SoftRejection("http_403", 1_000L, 0), state.softRejection)

        state.rejectOutbox("http_422")
        state.migrateRejectionPolicy(nowMs = 9_000L)
        assertEquals("marker already set: a later rejection is not reinterpreted", "http_422", state.outboxRejection)
    }

    @Test
    fun namedRejectionIsNeverConvertedToSoft() {
        state.advance(SyncPhase.LIVE)
        state.rejectOutbox("producer_not_authorized")

        state.migrateRejectionPolicy(nowMs = 1_000L)

        assertEquals("producer_not_authorized", state.outboxRejection)
        assertNull(state.softRejection)
    }

    @Test
    fun beginTooLargeMarkerBlocksSnapshotBeginUntilSixHours() {
        state.recordHistoryTooLarge(nowMs = 1_000L)

        assertFalse(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS - 1))
        assertTrue(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS))
        assertTrue("the query has no side effect", state.hasHistoryTooLarge)
        assertEquals(HistoryTooLargeKind.SNAPSHOT_BEGIN, state.historyTooLargeKind)
        state.clearHistoryTooLarge()
        assertFalse(state.hasHistoryTooLarge)
    }

    @Test
    fun byteCapTooLargeMarkerWaitsSevenDaysNotSixHours() {
        state.recordHistoryTooLarge(nowMs = 1_000L, kind = HistoryTooLargeKind.DOWNLOAD_BYTE_CAP)

        assertFalse(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS))
        assertFalse(state.snapshotAttemptAllowed(nowMs = 1_000L + SEVEN_DAYS_MS - 1))
        assertTrue(state.snapshotAttemptAllowed(nowMs = 1_000L + SEVEN_DAYS_MS))
        assertEquals(HistoryTooLargeKind.DOWNLOAD_BYTE_CAP, state.historyTooLargeKind)
    }

    @Test
    fun markerWithoutKindFieldIsTheSixHourBeginKind() {
        state.recordHistoryTooLarge(nowMs = 1_000L, kind = HistoryTooLargeKind.DOWNLOAD_BYTE_CAP)
        context.getSharedPreferences("peppy-gateway-state", Context.MODE_PRIVATE)
            .edit().remove("history-too-large-kind.v1").commit()

        assertEquals(HistoryTooLargeKind.SNAPSHOT_BEGIN, state.historyTooLargeKind)
        assertFalse(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS - 1))
        assertTrue(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS))
    }

    @Test
    fun versionChangeAuthorizesOneSnapshotBeginForEitherKind() {
        for (kind in HistoryTooLargeKind.entries) {
            state.clearHistoryTooLarge()
            state.recordHistoryTooLarge(nowMs = 1_000L, kind = kind)
            assertFalse(state.snapshotAttemptAllowed(nowMs = 2_000L))

            // The stored build version differs from this build; the store compares it on the next check.
            context.getSharedPreferences("peppy-gateway-state", Context.MODE_PRIVATE)
                .edit().putLong("history-too-large-version.v1", -1L).commit()

            assertTrue(kind.name, state.snapshotAttemptAllowed(nowMs = 2_000L))
        }
    }


    @Test
    fun newEnrollmentClearsTheTooLargeMarkerAndSetupProgress() {
        state.recordHistoryTooLarge(nowMs = 1_000L)
        state.setDownloading(5uL, 10uL)

        state.startBootstrap()

        assertFalse(state.hasHistoryTooLarge)
        assertNull(state.setupProgress)
    }

    @Test
    fun setupProgressIsPersistedPerPhaseAndClearedWhenLive() {
        state.setDownloading(200uL, 3_000uL)
        assertEquals(GatewayStateStore.SetupProgressState(GatewayStateStore.SetupPhase.DOWNLOADING, 200, 3_000, 0), state.setupProgress)

        state.setOrganizing(42uL)
        assertEquals(GatewayStateStore.SetupProgressState(GatewayStateStore.SetupPhase.ORGANIZING, 0, 0, 42), state.setupProgress)

        state.advance(SyncPhase.LIVE)
        assertNull(state.setupProgress)
    }

    @Test
    fun setupProgressIsNeverWrittenOutsideBootstrap() {
        state.advance(SyncPhase.LIVE)

        state.setDownloading(5uL, 10uL)

        assertNull(state.setupProgress)
        assertNull("nothing persisted", rawSetupPhase())
    }

    @Test
    fun setupProgressPersistedFromStaleFieldsIsIgnoredAndCleanedWhenLive() {
        state.setDownloading(5uL, 10uL)
        state.advance(SyncPhase.LIVE)
        // An older build left the fields behind: write them directly, as that build would have.
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit().putString("setup-phase.v1", "DOWNLOADING").commit()

        assertNull("a LIVE phone never reports stale setup progress", state.setupProgress)
        state.clearStaleSetupProgress()
        assertNull(rawSetupPhase())
    }

    @Test
    fun tooLargeRecordClearsPersistedSetupFields() {
        state.setDownloading(98_000uL, 108_882uL)

        state.recordHistoryTooLarge(nowMs = 1_000L, kind = HistoryTooLargeKind.DOWNLOAD_BYTE_CAP)

        assertNull(state.setupProgress)
        assertNull(rawSetupPhase())
    }

    @Test
    fun advancingToLiveClearsTheTooLargeMarker() {
        state.recordHistoryTooLarge(nowMs = 1_000L)

        state.advance(SyncPhase.LIVE)

        assertFalse(state.hasHistoryTooLarge)
    }

    @Test
    fun changesEmitForEveryPersistedWrite() = runBlocking {
        val changes = mutableListOf<Unit>()
        val collector = launch(Dispatchers.Unconfined) { state.changes().collect { changes += Unit } }

        state.setChecking()
        state.recordHistoryTooLarge(nowMs = 1_000L)

        assertTrue("one emission per write", changes.size >= 2)
        collector.cancel()
    }

    private fun rawSetupPhase(): String? =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).getString("setup-phase.v1", null)

    @Test
    fun requiringRecoveryClearsSetupProgress() {
        state.setChecking()

        state.requireRecovery(RecoveryReason.REUSED_DEVICE_IDENTITY)

        assertNull(state.setupProgress)
    }

    @Test
    fun recoveryRequiredIsNotMigrated() {
        state.requireRecovery(RecoveryReason.PRODUCER_CONFLICT)
        state.rejectOutbox("http_403")

        state.migrateRejectionPolicy(nowMs = 1_000L)

        assertEquals("http_403", state.outboxRejection)
        assertNull(state.softRejection)
    }
}
