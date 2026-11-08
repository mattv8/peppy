package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.peppy_mobile_bindings.NativeRcsEligibilityPublication

class RcsEligibilityPublisherTest {
    @Test fun `publishes only narrow unique conversations while current`() {
        val port = FakePort()
        RcsEligibilityPublisher(port, current = { true }, nowMs = { 42 })
            .publish(listOf("one", "one", "two"))

        assertEquals(listOf("one", "two"), port.published.single().first.map(NativeRcsEligibilityPublication::conversationId))
        assertEquals(42L, port.published.single().second)
    }

    @Test fun `retired publisher does not write`() {
        val port = FakePort()
        RcsEligibilityPublisher(port, current = { false }).publish(listOf("one"))
        assertEquals(emptyList<Pair<List<NativeRcsEligibilityPublication>, Long>>(), port.published)
    }

    private class FakePort : RcsEligibilityPublisherPort {
        val published = mutableListOf<Pair<List<NativeRcsEligibilityPublication>, Long>>()
        override fun publish(publications: List<NativeRcsEligibilityPublication>, nowMs: Long): UInt {
            published += publications to nowMs
            return publications.size.toUInt()
        }
    }
}
