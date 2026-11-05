package dev.peppy.mobile.googlemessages

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LinkSignInRuntimeTest {
    @After fun reset() = LinkSignInRuntime.resetForTest()

    @Test fun `only one activity holds the WebView lease`() {
        val first = LinkSignInRuntime.acquireLease()

        assertNotNull(first)
        assertNull(LinkSignInRuntime.acquireLease())
        LinkSignInRuntime.releaseLease(first!!)
        assertNotNull(LinkSignInRuntime.acquireLease())
    }

    @Test fun `queued pairing always clears the shared flag`() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        assertTrue(LinkSignInRuntime.submitPairing {
            entered.countDown()
            release.await()
        })
        assertTrue(entered.await(1, TimeUnit.SECONDS))
        assertTrue(LinkSignInRuntime.pairingInProgress())
        release.countDown()
        assertTrue(waitForPairingToFinish())
        assertFalse(LinkSignInRuntime.pairingInProgress())
    }

    @Test fun `throwing pairing clears the shared flag`() {
        assertTrue(LinkSignInRuntime.submitPairing { error("pair_failed") })

        assertTrue(waitForPairingToFinish())
        assertFalse(LinkSignInRuntime.pairingInProgress())
    }

    private fun waitForPairingToFinish(): Boolean {
        repeat(100) {
            if (!LinkSignInRuntime.pairingInProgress()) return true
            Thread.sleep(10)
        }
        return false
    }
}
