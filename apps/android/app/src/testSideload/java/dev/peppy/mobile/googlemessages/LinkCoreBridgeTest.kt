package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkCoreBridgeTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @After fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkCoreBridge.resetForTest()
    }

    @Test
    fun `stale unlinked observation is dropped after pre-start fence begins`() {
        val validationEntered = CountDownLatch(1)
        val allowValidation = CountDownLatch(1)
        val fenceBegun = CountDownLatch(1)
        val observationDone = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        LinkCoreBridgeTestHooks.beforeObservationValidation = {
            validationEntered.countDown()
            allowValidation.await()
        }
        LinkCoreBridgeTestHooks.preStartFenceBegun = { fenceBegun.countDown() }

        assertFalse(LinkRuntime.pairingInProgress.get())
        Thread {
            LinkCoreBridge.recordObservation(LinkCoreBridge.observationToken(), context, LinkStatus(0, "unlinked", false, false, false, 0, ""))
            observationDone.countDown()
        }.start()
        assertTrue(validationEntered.await(1, TimeUnit.SECONDS))
        val pairing = LinkRuntime.tryBeginPairing()!!
        val fence = Thread { LinkCoreBridge.recordPreStart(context) }.also { it.start() }
        assertTrue(fenceBegun.await(1, TimeUnit.SECONDS))
        allowValidation.countDown()
        assertTrue(observationDone.await(1, TimeUnit.SECONDS))
        fence.join(1_000)

        assertEquals(listOf("unknown"), recorded)
        assertTrue(LinkRuntime.endPairing(pairing))
    }

    @Test
    fun `unlinked observation read before a completed pairing fence is dropped`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }

        val token = LinkCoreBridge.observationToken()
        val pairing = LinkRuntime.tryBeginPairing()!!
        assertTrue(LinkCoreBridge.recordPreStart(context))
        assertTrue(LinkRuntime.endPairing(pairing))

        assertTrue(LinkCoreBridge.recordObservation(token, context, LinkStatus(0, "unlinked", false, false, false, 0, "")))

        assertEquals(listOf("unknown"), recorded)
    }
}
