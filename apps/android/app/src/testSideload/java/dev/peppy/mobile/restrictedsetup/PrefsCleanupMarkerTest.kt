package dev.peppy.mobile.restrictedsetup

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PrefsCleanupMarkerTest {
    @Test fun `marker persists until acknowledged`() {
        val marker = PrefsCleanupMarker(RuntimeEnvironment.getApplication())

        marker.acknowledgeCleanup()
        assertFalse(marker.needsCleanup())
        assertTrue(marker.markCleanupNeeded())
        assertTrue(marker.needsCleanup())
        assertTrue(marker.acknowledgeCleanup())
        assertFalse(marker.needsCleanup())
    }
}
