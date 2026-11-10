package dev.peppy.mobile.googlemessages

import android.app.Activity
import android.os.Looper
import android.view.View
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.WebView
import android.widget.Button
import android.widget.TextView
import dev.peppy.mobile.R
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ActivityController
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
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
        LinkRuntimeTestHooks.eventRecorder = null
        LinkCoreBridge.resetForTest()
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

    @Test
    fun `sign-in reads link status off the main thread`() {
        val startup = ControlledStartup()
        val statusReadOnMain = AtomicBoolean()
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { true }
        LinkRuntimeTestHooks.engine = availableEngine(object : LinkedHandle() {
            override fun status(): String {
                statusReadOnMain.set(Thread.currentThread() === Looper.getMainLooper().thread)
                return super.status()
            }
        })
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        startup.run()
        shadowOf(Looper.getMainLooper()).idle()

        assertFalse(statusReadOnMain.get())
        controller.destroy()
    }

    @Test
    fun `sign-in screen keeps Cancel without Google blocked sign-in`() {
        val startup = ControlledStartup()
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { true }
        LinkRuntimeTestHooks.engine = availableEngine(object : LinkedHandle() {
            override fun status() = "{\"state\":\"unlinked\"}"
        })
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        val activity = controller.get()
        startup.run()
        shadowOf(Looper.getMainLooper()).idle()

        val buttonTexts = descendantViews(activity.window.decorView)
            .filterIsInstance<Button>()
            .map { it.text.toString() }
        assertTrue(buttonTexts.contains("Cancel"))
        assertFalse(buttonTexts.contains("Google blocked sign-in"))
        controller.destroy()
    }

    @Test
    fun `finished config page with sign-in cookies starts pairing without a confirmation screen`() {
        val pairing = QueueExecutor()
        val pairCalls = AtomicInteger()
        val controller = startSignInWithPairableLink(pairing, onPair = { pairCalls.incrementAndGet() })
        val activity = controller.get()
        seedSignInCookies()

        finishConfigPage(activity)

        assertFinishedWith(activity, "pairing_started")
        assertEquals(activity.getString(R.string.peppy_google_messages_link_signin_finishing), loadingText(activity))
        assertEquals(1, pairing.size)
        assertEquals(0, pairCalls.get())
        pairing.runAll()
        assertEquals(1, pairCalls.get())
        assertNull(pendingCookiesOf(activity))
        assertNextLeaseAvailable()
        controller.destroy()
    }

    @Test
    fun `second config page finish does not queue a second pairing`() {
        val pairing = QueueExecutor()
        val controller = startSignInWithPairableLink(pairing)
        val activity = controller.get()
        seedSignInCookies()
        val webView = webViewOf(activity)

        firePageFinished(webView)
        firePageFinished(webView)

        assertEquals(1, pairing.size)
        controller.destroy()
    }

    @Test
    fun `back after cookie capture does not start a second pairing or change the outcome`() {
        val pairing = QueueExecutor()
        val pairCalls = AtomicInteger()
        val controller = startSignInWithPairableLink(pairing, onPair = { pairCalls.incrementAndGet() })
        val activity = controller.get()
        seedSignInCookies()
        finishConfigPage(activity)

        activity.onBackPressedDispatcher.onBackPressed()
        shadowOf(Looper.getMainLooper()).idle()

        assertFinishedWith(activity, "pairing_started")
        assertEquals(1, pairing.size)
        pairing.runAll()
        assertEquals(1, pairCalls.get())
        controller.destroy()
    }

    @Test
    fun `back during a pending pairing handoff cancels without queueing pairing`() {
        val pairing = QueueExecutor()
        val controller = startSignInWithPairableLink(pairing)
        val activity = controller.get()
        seedSignInCookies()
        LinkSignInActivity::class.java.getDeclaredField("pendingCookies").apply {
            isAccessible = true
            set(activity, FilteredCookies(emptyMap()))
        }

        activity.onBackPressedDispatcher.onBackPressed()
        shadowOf(Looper.getMainLooper()).idle()

        assertFinishedWith(activity, "cancelled")
        assertEquals(0, pairing.size)
        assertTrue(CookieManager.getInstance().getCookie(CONFIG_URL).isNullOrEmpty())
        assertNextLeaseAvailable()
        controller.destroy()
    }

    @Test
    fun `sign-in cookies are wiped before pairing is queued`() {
        var cookiesAtQueue: String? = null
        val pairing = QueueExecutor(onExecute = { cookiesAtQueue = CookieManager.getInstance().getCookie(CONFIG_URL) })
        val controller = startSignInWithPairableLink(pairing)
        val activity = controller.get()
        seedSignInCookies()

        finishConfigPage(activity)

        assertEquals(1, pairing.size)
        assertTrue(cookiesAtQueue.isNullOrEmpty())
        controller.destroy()
    }

    @Test
    fun `finished config page without sign-in cookies finishes cookies_missing without pairing`() {
        val pairing = QueueExecutor()
        val controller = startSignInWithPairableLink(pairing)
        val activity = controller.get()

        finishConfigPage(activity)

        assertFinishedWith(activity, "cookies_missing")
        assertEquals(0, pairing.size)
        controller.destroy()
    }

    @Test
    fun `cancel during sign-in wipes cookies and finishes cancelled without pairing`() {
        val pairing = QueueExecutor()
        val controller = startSignInWithPairableLink(pairing)
        val activity = controller.get()
        seedSignInCookies()

        descendantViews(activity.window.decorView)
            .filterIsInstance<Button>()
            .single { it.text.toString() == "Cancel" }
            .performClick()
        shadowOf(Looper.getMainLooper()).idle()

        assertFinishedWith(activity, "cancelled")
        assertTrue(CookieManager.getInstance().getCookie(CONFIG_URL).isNullOrEmpty())
        assertEquals(0, pairing.size)
        controller.destroy()
    }

    @Test
    fun `queued retired pairing cannot publish prestart or open a replacement handle`() {
        val startup = ControlledStartup()
        val pairing = QueueExecutor()
        val recorded = mutableListOf<String>()
        val diagnostics = mutableListOf<String>()
        val handle = PairCountingHandle()
        var opens = 0
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { false }
        LinkSignInRuntimeTestHooks.pairingExecutor = pairing
        LinkCoreBridge.recorder = { _, status -> recorded += status.state; true }
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle.also { opens++ }
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }
        LinkRuntimeTestHooks.eventRecorder = { kind, code, _ -> diagnostics += "$kind/$code" }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        val activity = controller.get()
        queuePairing(activity)
        LinkRuntime.resetLocalState(activity)
        pairing.runAll()

        assertEquals(listOf("unlinked"), recorded)
        assertEquals(0, opens)
        assertEquals(0, handle.pairCalls.get())
        assertEquals(listOf("link/reset_local", "link/persist_failed"), diagnostics)
        controller.destroy()
    }

    @Test
    fun `current queued pairing publishes prestart before native pair`() {
        val startup = ControlledStartup()
        val pairing = QueueExecutor()
        val order = mutableListOf<String>()
        val handle = object : LinkedHandle() {
            override fun pair(cookiesJson: ByteArray) { order += "pair" }
        }
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { false }
        LinkSignInRuntimeTestHooks.pairingExecutor = pairing
        LinkCoreBridge.recorder = { _, status -> order += status.state; true }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        queuePairing(controller.get())
        pairing.runAll()

        assertEquals(listOf("unknown", "pair"), order)
        controller.destroy()
    }

    @Test
    fun `failed lifecycle fence prevents native pair and records persistence failure`() {
        val startup = ControlledStartup()
        val handle = PairCountingHandle()
        val persistFailure = CountDownLatch(1)
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { false }
        LinkCoreBridge.recorder = { _, _ -> false }
        LinkRuntimeTestHooks.engine = availableEngine(handle)
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }
        LinkRuntimeTestHooks.eventRecorder = { _, code, _ -> if (code == "persist_failed") persistFailure.countDown() }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        val activity = controller.get()
        startup.run()
        LinkSignInActivity::class.java.getDeclaredField("pendingCookies").apply {
            isAccessible = true
            set(activity, FilteredCookies(emptyMap()))
        }
        LinkSignInActivity::class.java.getDeclaredMethod("startPairing").apply {
            isAccessible = true
            invoke(activity)
        }

        assertTrue(persistFailure.await(1, TimeUnit.SECONDS))
        awaitPairingCompletion()
        assertEquals(0, handle.pairCalls.get())
        controller.destroy()
    }

    private fun startSignInWithPairableLink(
        pairing: QueueExecutor,
        onPair: () -> Unit = {},
    ): ActivityController<LinkSignInActivity> {
        val startup = ControlledStartup()
        LinkSignInActivityTestHooks.startupExecutor = startup::capture
        LinkSignInActivityTestHooks.awaitSignInReady = { true }
        LinkSignInRuntimeTestHooks.pairingExecutor = pairing
        LinkCoreBridge.recorder = { _, _ -> true }
        LinkRuntimeTestHooks.engine = availableEngine(object : LinkedHandle() {
            override fun status() = """{"state":"unlinked"}"""
            override fun pair(cookiesJson: ByteArray) = onPair()
        })
        LinkRuntimeTestHooks.storeFactory = { EmptyLinkStateStorage }

        val controller = Robolectric.buildActivity(LinkSignInActivity::class.java).setup()
        startup.run()
        shadowOf(Looper.getMainLooper()).idle()
        return controller
    }

    // Robolectric's CookieManager keeps only the first "name=value" pair of each setCookie call.
    private fun seedSignInCookies() {
        CookieManager.getInstance().apply {
            SIGN_IN_COOKIES.forEach { setCookie(CONFIG_URL, "$it; Secure") }
        }
    }

    private fun finishConfigPage(activity: LinkSignInActivity) = firePageFinished(webViewOf(activity))

    private fun firePageFinished(webView: WebView) {
        shadowOf(webView).webViewClient.onPageFinished(webView, CONFIG_URL)
        shadowOf(Looper.getMainLooper()).idle()
    }

    private fun webViewOf(activity: LinkSignInActivity): WebView =
        descendantViews(activity.window.decorView).filterIsInstance<WebView>().single()

    private fun loadingText(activity: LinkSignInActivity): String =
        descendantViews(activity.window.decorView)
            .filterIsInstance<TextView>()
            .single { it.id == R.id.google_messages_link_signin_loading }
            .text.toString()

    private fun pendingCookiesOf(activity: LinkSignInActivity): Any? =
        LinkSignInActivity::class.java.getDeclaredField("pendingCookies").run {
            isAccessible = true
            get(activity)
        }

    private fun queuePairing(activity: LinkSignInActivity) {
        LinkSignInActivity::class.java.getDeclaredField("pendingCookies").apply {
            isAccessible = true
            set(activity, FilteredCookies(emptyMap()))
        }
        LinkSignInActivity::class.java.getDeclaredMethod("startPairing").apply {
            isAccessible = true
            invoke(activity)
        }
    }

    private fun awaitPairingCompletion() {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(1)
        while (LinkRuntime.pairingInProgress.get() && System.nanoTime() < deadline) Thread.yield()
        assertFalse(LinkRuntime.pairingInProgress.get())
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

    private fun descendantViews(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) {
            for (index in 0 until view.childCount) yieldAll(descendantViews(view.getChildAt(index)))
        }
    }

    private fun availableEngine(handle: LinkHandle) = object : LinkEngine {
        override val available = true
        override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
    }

    private class QueueExecutor(private val onExecute: () -> Unit = {}) : Executor {
        private val tasks = ArrayDeque<Runnable>()
        val size: Int get() = tasks.size
        override fun execute(command: Runnable) {
            onExecute()
            tasks.addLast(command)
        }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
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

    private open class LinkedHandle : LinkHandle {
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth() = 0L
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() = Unit
        override fun status() = """{"state":"linked"}"""
    }

    private class PairCountingHandle : LinkedHandle() {
        val pairCalls = AtomicInteger()
        override fun pair(cookiesJson: ByteArray) { pairCalls.incrementAndGet() }
    }

    private object EmptyLinkStateStorage : LinkStateStorage {
        override fun read(): ByteArray = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }

    private companion object {
        const val CONFIG_URL = "https://messages.google.com/web/config"
        val SIGN_IN_COOKIES = listOf("OSID=o", "SID=s", "HSID=h", "SSID=ss", "APISID=a", "SAPISID=sa")
    }
}
