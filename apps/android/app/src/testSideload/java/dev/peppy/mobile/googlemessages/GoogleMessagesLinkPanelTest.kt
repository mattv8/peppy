package dev.peppy.mobile.googlemessages

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanel
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanelHost
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.After
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkPanelTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @After fun tearDown() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        GoogleMessagesLinkTestHooks.available = null
    }

    @Test fun `unavailable engine renders nothing`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = false
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("unavailable")
        }
        GoogleMessagesLinkTestHooks.available = false
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        check(compose.onAllNodesWithTag("google-messages-link-panel").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `unlinked link offers owner action`() = panel("unlinked") {
        compose.onNodeWithTag("google-messages-link-unlinked").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-start").fetchSemanticsNode()
    }

    @Test fun `link consent shows the complete disclosure`() = panel("unlinked") {
        compose.onNodeWithTag("google-messages-link-start").performClick()
        compose.onNodeWithTag("google-messages-link-consent-sheet").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-consent-sheet-body").fetchSemanticsNode()
    }

    @Test fun `linked link presents health actions`() = panel("linked") {
        compose.onNodeWithTag("google-messages-link-linked").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-check-now").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-battery-row").fetchSemanticsNode()
    }

    @Test fun `repair state offers owner actions`() {
        panel("needs_relink") {
            compose.onNodeWithTag("google-messages-link-relink").fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
        }
    }

    @Test fun `unlink and remove require confirmation`() {
        panel("needs_relink") {
            compose.onNodeWithTag("google-messages-link-unlink").performClick()
            compose.onNodeWithTag("google-messages-link-unlink-sheet").fetchSemanticsNode()
        }
    }

    @Test fun `health check reports a completed result`() = panel("linked") {
        compose.onNodeWithTag("google-messages-link-check-now").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-result").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun `signed out state never offers unlink`() = panel("signed_out") {
        compose.onNodeWithTag("google-messages-link-sign-in-again").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-unlink").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `unreadable state offers reset only`() {
        GoogleMessagesLinkTestHooks.available = true
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("unreadable")
        }
        LinkRuntimeTestHooks.storeFactory = { object : LinkStateStorage {
            override fun read(): ByteArray = throw IllegalStateException("unreadable")
            override fun write(state: ByteArray) = Unit
            override fun wipe() = Unit
        } }
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-reset").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-reset").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-start").fetchSemanticsNodes().isEmpty())
    }


    @Test fun `older load and reset completion cannot replace newer status`() {
        val firstEntered = CountDownLatch(1)
        val releaseFirst = CountDownLatch(1)
        val resetLoadEntered = CountDownLatch(1)
        val releaseResetLoad = CountDownLatch(1)
        val mode = AtomicInteger(0)
        val staleFirst = blockingHandle("needs_relink", firstEntered, releaseFirst)
        val staleReset = blockingHandle("needs_relink", resetLoadEntered, releaseResetLoad)
        val current = FakeHandle("linked")
        val host = FakePanelHost(
            handle = current,
            openResult = {
                when (mode.get()) {
                    0 -> if (firstEntered.count == 1L) LinkOpenResult.Open(staleFirst) else LinkOpenResult.Open(current)
                    1 -> LinkOpenResult.Unreadable("test")
                    2 -> LinkOpenResult.Open(staleReset)
                    else -> LinkOpenResult.Open(current)
                }
            },
            onReset = { mode.set(3); current.state = "unlinked" },
        )
        showPanel(host)
        check(firstEntered.await(2, TimeUnit.SECONDS))

        host.signalChange()
        compose.waitUntil(5_000) { compose.onAllNodesWithTag("google-messages-link-linked").fetchSemanticsNodes().isNotEmpty() }
        releaseFirst.countDown()
        compose.waitForIdle()
        compose.onNodeWithTag("google-messages-link-linked").fetchSemanticsNode()

        mode.set(1)
        host.signalChange()
        compose.waitUntil(5_000) { compose.onAllNodesWithTag("google-messages-link-reset").fetchSemanticsNodes().isNotEmpty() }
        mode.set(2)
        host.signalChange()
        compose.waitUntil(5_000) { resetLoadEntered.count == 0L }
        compose.onNodeWithTag("google-messages-link-reset").performClick()
        compose.onNodeWithTag("google-messages-link-reset-sheet-confirm").performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil(5_000) { compose.onAllNodesWithTag("google-messages-link-start").fetchSemanticsNodes().isNotEmpty() }

        releaseResetLoad.countDown()
        compose.waitForIdle()
        compose.onNodeWithTag("google-messages-link-start").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isEmpty())
        check(host.resetCalls == 1)
    }

    @Test fun `permission continuations never cross from notification or relink into sign in`() {
        val host = FakePanelHost(FakeHandle("needs_relink"), notificationsGranted = false)
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-relink").performClick()
        compose.onNodeWithTag("google-messages-link-relink-sheet-confirm").performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil(5_000) { host.permissionRequests == 1 }
        check(host.serviceStarts == 0)

        host.completePermissionRequest(granted = false)
        host.completePermissionRequest(granted = true)
        compose.waitUntil { host.relinkCalls == 1 }
        check(host.signInLaunches == 0)
        check(host.serviceStarts == 1)

        host.handle.state = "pairing"
        host.notificationsGranted = false
        host.signalChange()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notifications-allow").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notifications-allow").performClick()
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.signInLaunches == 0)
        check(host.serviceStarts == 1)
    }

    @Test
    @Config(sdk = [35], qualifiers = "w320dp-h480dp")
    fun `large text keeps actions reachable and exposes merged live regions`() {
        val host = FakePanelHost(FakeHandle("linked"), notificationsGranted = true)
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                Column(Modifier.width(240.dp).height(320.dp).verticalScroll(rememberScrollState())) {
                    PeppyTheme { GoogleMessagesLinkPanel(host) {} }
                }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-now").fetchSemanticsNodes().isNotEmpty() }
        val polite = SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite)
        compose.onNodeWithTag("google-messages-link-status")
            .assert(polite).assertTextContains("Status").assertTextContains("Linked")
        compose.onNodeWithTag("google-messages-link-check-now").performScrollTo().assertIsDisplayed().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-result").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-check-result").assert(polite)
        compose.onNodeWithTag("google-messages-link-unlink").performScrollTo().assertIsDisplayed().performClick()
        compose.onNodeWithTag("google-messages-link-unlink-sheet-confirm").assertIsDisplayed()

        host.handle.state = "pairing"
        host.emojiFlow.value = "😀"
        host.signalChange()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-emoji-card").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-emoji-card")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Assertive))
            .assertTextContains("😀", substring = true)
        check(compose.onAllNodesWithTag("google-messages-link-emoji", useUnmergedTree = false).fetchSemanticsNodes().isEmpty())
    }

    private fun panel(state: String, assertion: () -> Unit) {
        GoogleMessagesLinkTestHooks.available = true
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(stateBytes: ByteArray, host: LinkHost): LinkHandle = object : LinkHandle {
                override fun pair(cookiesJson: ByteArray) = Unit
                override fun relink() = Unit
                override fun run() = Unit
                override fun checkHealth() = 1L
                override fun unlink() = Unit
                override fun stop() = Unit
                override fun close() = Unit
                override fun status() = """{"seq":1,"state":"$state","running":true,"healthy":true,"phone_responding":true,"last_ok_ms":0,"stop_code":""}"""
            }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-panel").fetchSemanticsNodes().isNotEmpty() }
        assertion()
    }


    private fun showPanel(host: FakePanelHost) {
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel(host) {} } }
    }

    private open class FakeHandle(var state: String) : LinkHandle {
        var relinkCalls = 0
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() { relinkCalls++ }
        override fun run() = Unit
        override fun checkHealth() = 1L
        override fun unlink() = Unit
        override fun stop() = Unit
        override fun close() = Unit
        override fun status() = statusJson(state)
    }

    private class FakePanelHost(
        val handle: FakeHandle,
        override var notificationsGranted: Boolean = true,
        private val openResult: () -> LinkOpenResult = { LinkOpenResult.Open(handle) },
        private val onReset: () -> Unit = { handle.state = "unlinked" },
    ) : GoogleMessagesLinkPanelHost {
        private val changesFlow = MutableStateFlow(0L)
        override val changes: StateFlow<Long> = changesFlow
        val emojiFlow = MutableStateFlow<String?>(null)
        override val emoji: StateFlow<String?> = emojiFlow
        private var permissionContinuation: (() -> Unit)? = null
        var permissionRequests = 0
        var signInLaunches = 0
        var serviceStarts = 0
        var resetCalls = 0
        val relinkCalls get() = handle.relinkCalls

        override fun open(): LinkOpenResult = openResult()
        override fun resetLocalState() { resetCalls++; onReset() }
        override fun pairingInProgress(): Boolean = LinkRuntime.pairingInProgress.get()
        override fun beginPairing(): Boolean = LinkRuntime.beginPairing()
        override fun endPairing() { LinkRuntime.endPairing() }
        override fun requestNotificationPermission(afterResult: () -> Unit) {
            permissionRequests++
            permissionContinuation = afterResult
        }
        override fun launchSignIn(onResult: (String?) -> Unit) { signInLaunches++ }
        override fun startService() { serviceStarts++ }

        fun completePermissionRequest(granted: Boolean = notificationsGranted) {
            notificationsGranted = granted
            permissionContinuation?.also { permissionContinuation = null }?.invoke()
        }
        fun signalChange() { changesFlow.value++ }
    }

    private fun blockingHandle(
        state: String,
        entered: CountDownLatch,
        release: CountDownLatch,
    ) = object : FakeHandle(state) {
        override fun status(): String {
            entered.countDown()
            while (true) {
                try {
                    if (release.await(20, TimeUnit.MILLISECONDS)) return super.status()
                } catch (_: InterruptedException) {
                    // Simulates a native call that does not cooperate with coroutine cancellation.
                }
            }
        }
    }

    private companion object {
        fun statusJson(state: String) = """{"seq":1,"state":"$state","running":true,"healthy":true,"phone_responding":true,"last_ok_ms":0,"stop_code":""}"""
    }

    private class InMemoryStore : LinkStateStorage {
        override fun read() = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }
}
