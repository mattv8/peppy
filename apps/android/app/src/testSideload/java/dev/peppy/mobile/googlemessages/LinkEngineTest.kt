package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkEngineTest {
    @Test
    fun `parse retains empty stop code and sequence`() {
        val status = LinkStatus.parse("""{"seq":7,"state":"linked","stop_code":"","ignored":true}""")

        assertEquals(7, status.seq)
        assertEquals("", status.stopCode)
    }

    @Test
    fun `parse malformed JSON fails closed`() {
        val status = LinkStatus.parse("{")

        assertEquals("unknown", status.state)
        assertEquals("unknown", status.stopCode)
    }
}
