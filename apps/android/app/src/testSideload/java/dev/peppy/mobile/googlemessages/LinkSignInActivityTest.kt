package dev.peppy.mobile.googlemessages

import android.app.Activity
import android.os.Looper
import android.webkit.CookieManager
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkSignInActivityTest {
    @After
    fun reset() {
        LinkSignInActivityTestHooks.reset()
        LinkSignInRuntime.resetForTest()
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
    }

    @Test
    fun `failed startup wipe finishes blocked and releases untouched lease without jar access`() {
        val startup = ControlledStartup()
        val cookieManagerRequests = AtomicInteger()
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { false }
        LinkSignInActivityTestHooks.cookieManager = {
            cookieManagerRequests.incrementAndGet()
            CookieManager.getInstance()
        }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        val activity = controller.get()
        startup.run()
        shadowOf(Looper.getMainLooper()).idle()

        assertFinishedWith(activity, "blocked")
        assertEquals(0, cookieManagerRequests.get())
        assertNextLeaseAvailable()
        controller.destroy()
    }

    @Test
    fun `non-pairable existing link finishes error and releases untouched lease without jar access`() {
        val startup = ControlledStartup()
        val cookieManagerRequests = AtomicInteger()
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { true }
        LinkSignInActivityTestHooks.cookieManager = {
            cookieManagerRequests.incrementAndGet()
            CookieManager.getInstance()
        }
        LinkRuntimeTestHooks.engine = availableEngine(LinkedHandle())
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        val activity = controller.get()
        startup.run()
        shadowOf(Looper.getMainLooper()).idle()

        assertFinishedWith(activity, "error")
        assertEquals(0, cookieManagerRequests.get())
        assertNextLeaseAvailable()
        controller.destroy()
    }

    private fun assertFinishedWith(activity: LinkSignInActivity, outcome: String) {
        val shadow = shadowOf(activity)
        assertTrue(activity.isFinishing)
        assertEquals(Activity.RESULT_OK, shadow.resultCode)
        assertEquals(outcome, shadow.resultIntent.getStringExtra("dev.peppy.mobile.googlemessages.outcome"))
    }

    private fun assertNextLeaseAvailable() {
        val next = LinkSignInRuntime.acquireLease()
        assertNotNull(next)
        LinkSignInRuntime.releaseLease(requireNotNull(next))
    }

    private fun availableEngine(handle: LinkHandle) = object : LinkEngine {
        override val available = true
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
    }

    private class ControlledStartup {
        private val captured = CountDownLatch(1)
        private lateinit var task: () -> Unit

        fun capture(startup: () -> Unit) {
            task = startup
            captured.countDown()
        }

        fun run() {
            assertTrue("startup was not scheduled", captured.await(1, TimeUnit.SECONDS))
            Thread(task, "link-sign-in-test-startup").apply {
                start()
                join(1_000)
                assertTrue("startup did not finish", !isAlive)
            }
        }
    }

    private class LinkedHandle : LinkHandle {
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth() = 0L
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() = Unit
        override fun status() = """{"state":"linked"}"""
    }

    private object EmptyLinkStateStorage : LinkStateStorage {
        override fun read(): ByteArray = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }
}
