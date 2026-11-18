package dev.peppy.mobile.gateway

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** minSdk is 26: `PackageInfo.longVersionCode` does not exist there, so the marker must use the compat lookup. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [26])
class GatewayStateStoreApi26Test {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val state get() = GatewayStateStore(context)

    @Test
    fun tooLargeMarkerRecordsAndRechecksOnApi26() {
        state.recordHistoryTooLarge(nowMs = 1_000L)

        assertTrue(state.hasHistoryTooLarge)
        assertFalse(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS - 1))
        assertTrue(state.snapshotAttemptAllowed(nowMs = 1_000L + SIX_HOURS_MS))
    }

    private companion object {
        const val SIX_HOURS_MS = 6 * 60 * 60 * 1000L
    }
}
