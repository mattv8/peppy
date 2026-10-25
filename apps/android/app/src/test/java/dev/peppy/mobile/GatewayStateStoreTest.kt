package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayStateStoreTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val state get() = GatewayStateStore(context)

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
    fun recoveryRequiredIsNotMigrated() {
        state.requireRecovery(RecoveryReason.PRODUCER_CONFLICT)
        state.rejectOutbox("http_403")

        state.migrateRejectionPolicy(nowMs = 1_000L)

        assertEquals("http_403", state.outboxRejection)
        assertNull(state.softRejection)
    }
}
