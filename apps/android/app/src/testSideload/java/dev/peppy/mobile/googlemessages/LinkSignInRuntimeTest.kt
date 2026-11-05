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
    @After fun reset() {
        LinkSignInRuntime.resetForTest()
    }

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
        val attempt = requireNotNull(LinkSignInRuntime.beginPairing())
        assertTrue(LinkSignInRuntime.executePairing(attempt) {
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
        val attempt = requireNotNull(LinkSignInRuntime.beginPairing())
        assertTrue(LinkSignInRuntime.executePairing(attempt) { error("pair_failed") })

        assertTrue(waitForPairingToFinish())
        assertFalse(LinkSignInRuntime.pairingInProgress())
    }

    @Test fun `failed cleanup poisons before releasing the WebView lease`() {
        val lease = requireNotNull(LinkSignInRuntime.acquireLease())

        assertTrue(LinkSignInRuntime.completeCleanup(lease, success = false))
        assertTrue(LinkSignInRuntime.isSignInBlocked())
        assertNull(LinkSignInRuntime.acquireLease())
    }

    @Test fun `cleanup callback retains WebView ownership until it completes`() {
        val first = requireNotNull(LinkSignInRuntime.acquireLease())
        assertTrue(LinkSignInRuntime.markWebAccess(first))
        assertTrue(LinkSignInRuntime.beginCleanup(first))

        assertNull(LinkSignInRuntime.acquireLease())
        assertTrue(LinkSignInRuntime.mayRunCleanup(first))
        assertTrue(LinkSignInRuntime.completeCleanup(first, success = true))

        val second = requireNotNull(LinkSignInRuntime.acquireLease())
        assertFalse(LinkSignInRuntime.mayRunCleanup(first))
        LinkSignInRuntime.releaseLease(second)
    }

    private fun waitForPairingToFinish(): Boolean {
        repeat(100) {
            if (!LinkSignInRuntime.pairingInProgress()) return true
            Thread.sleep(10)
        }
        return false
    }
}
