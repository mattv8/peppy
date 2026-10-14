package dev.peppy.mobile

import android.content.Context
import android.content.ContextWrapper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
class SubscriptionSmsManagerTest {
    @Test
    @Config(sdk = [30])
    fun legacyFactoryReturnsManagerForRequestedSubscription() {
        val manager = subscriptionSmsManager(RuntimeEnvironment.getApplication(), 41)

        assertEquals(41, manager.subscriptionId)
    }

    @Test
    @Config(sdk = [31])
    fun systemServiceFactoryReturnsManagerForRequestedSubscription() {
        val manager = subscriptionSmsManager(RuntimeEnvironment.getApplication(), 42)

        assertEquals(42, manager.subscriptionId)
    }

    @Test
    @Config(sdk = [31])
    fun systemServiceFailureIsPropagated() {
        assertThrows(IllegalStateException::class.java) { subscriptionSmsManager(failingServiceContext(), 43) }
    }

    @Test
    @Config(sdk = [31])
    fun mmsLimitsFallsBackWhenSystemServiceFails() {
        val limits = MmsLimits.forSubscription(failingServiceContext(), 44)

        assertEquals(MmsLimits(MmsLimits.FALLBACK_PDU_BYTES, 20, true, MmsLimits.Source.FALLBACK), limits)
    }

    private fun failingServiceContext(): Context =
        object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun getSystemService(name: String): Any {
                throw IllegalStateException("unavailable")
            }
        }
}
