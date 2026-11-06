package dev.peppy.mobile.googlemessages

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanel
import dev.peppy.mobile.R
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanelHost
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.theme.PeppyTheme
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
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
        val firstCompleted = CountDownLatch(1)
        val resetLoadEntered = CountDownLatch(1)
        val releaseResetLoad = CountDownLatch(1)
        val resetLoadCompleted = CountDownLatch(1)
        val mode = AtomicInteger(0)
        val staleFirst = blockingHandle("needs_relink", firstEntered, releaseFirst, firstCompleted)
        val staleReset = blockingHandle("needs_relink", resetLoadEntered, releaseResetLoad, resetLoadCompleted)
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
        compose.waitUntil(5_000) { firstCompleted.count == 0L }
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
        compose.waitUntil(5_000) { resetLoadCompleted.count == 0L }
        compose.onNodeWithTag("google-messages-link-start").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isEmpty())
        check(host.resetCalls == 1)
    }

    @Test fun `pending link permission survives configuration stop save recreate exactly once`() {
        val host = FakePanelHost(FakeHandle("unlinked"), notificationsGranted = false)
        val restoration = StateRestorationTester(compose)
        var owner: LifecycleOwner = resumedOwner()
        GoogleMessagesLinkTestHooks.available = true
        restoration.setContent {
            CompositionLocalProvider(LocalLifecycleOwner provides owner) {
                PeppyTheme { GoogleMessagesLinkPanel(host) {} }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-start").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-start").performClick()
        compose.onNodeWithTag("google-messages-link-consent-sheet-confirm")
            .performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil { host.permissionRequests == 1 }

        host.isChangingConfigurations = true
        compose.runOnIdle { (owner.lifecycle as LifecycleRegistry).handleLifecycleEvent(Lifecycle.Event.ON_STOP) }
        owner = resumedOwner()
        restoration.emulateSavedInstanceStateRestore()
        host.isChangingConfigurations = false
        host.completePermissionRequest(granted = false)
        compose.waitUntil { host.signInLaunches == 1 }
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.signInLaunches == 1)
        check(host.serviceStarts == 0)
    }

    @Test fun `pending relink permission survives configuration stop save recreate exactly once`() {
        val host = FakePanelHost(FakeHandle("needs_relink"), notificationsGranted = false)
        val restoration = StateRestorationTester(compose)
        var owner: LifecycleOwner = resumedOwner()
        GoogleMessagesLinkTestHooks.available = true
        restoration.setContent {
            CompositionLocalProvider(LocalLifecycleOwner provides owner) {
                PeppyTheme { GoogleMessagesLinkPanel(host) {} }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isNotEmpty() }
        confirm("google-messages-link-relink")
        compose.waitUntil { host.permissionRequests == 1 }

        host.isChangingConfigurations = true
        compose.runOnIdle { (owner.lifecycle as LifecycleRegistry).handleLifecycleEvent(Lifecycle.Event.ON_STOP) }
        owner = resumedOwner()
        restoration.emulateSavedInstanceStateRestore()
        host.isChangingConfigurations = false
        host.completePermissionRequest(granted = false)
        compose.waitUntil { host.relinkCalls == 1 }
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.relinkCalls == 1)
        check(host.serviceStarts == 1)
        check(host.signInLaunches == 0)
    }

    @Test fun `stopping while permission is pending cancels the confirmed action`() {
        val host = FakePanelHost(FakeHandle("unlinked"), notificationsGranted = false)
        val owner = object : LifecycleOwner {
            override val lifecycle = LifecycleRegistry(this).apply { currentState = Lifecycle.State.RESUMED }
        }
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            CompositionLocalProvider(LocalLifecycleOwner provides owner) {
                PeppyTheme { GoogleMessagesLinkPanel(host) {} }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-start").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-start").performClick()
        compose.onNodeWithTag("google-messages-link-consent-sheet-confirm")
            .performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil { host.permissionRequests == 1 }

        compose.runOnIdle { (owner.lifecycle as LifecycleRegistry).handleLifecycleEvent(Lifecycle.Event.ON_STOP) }
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.signInLaunches == 0)
    }

    @Test fun `relink completion carries its pairing token and cannot end a newer attempt`() {
        val relinkEntered = CountDownLatch(1)
        val releaseRelink = CountDownLatch(1)
        val relinkCompleted = CountDownLatch(1)
        val handle = FakeHandle("needs_relink").apply {
            relinkAction = {
                relinkEntered.countDown()
                awaitIgnoringInterrupts(releaseRelink)
                relinkCompleted.countDown()
            }
        }
        val host = FakePanelHost(handle, notificationsGranted = false)
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-relink").performClick()
        compose.onNodeWithTag("google-messages-link-relink-sheet-confirm")
            .performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil { host.permissionRequests == 1 }
        host.completePermissionRequest(granted = false)
        compose.waitUntil(5_000) { relinkEntered.count == 0L && host.pairingTokens.isNotEmpty() }
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.relinkCalls == 1)
        check(host.serviceStarts == 1)
        check(host.signInLaunches == 0)

        val first = host.pairingTokens.single()
        assertTrue(LinkRuntime.endPairing(first))
        val second = checkNotNull(LinkRuntime.tryBeginPairing())
        releaseRelink.countDown()
        compose.waitUntil(5_000) { relinkCompleted.count == 0L && host.endedTokens.contains(first) }

        assertTrue(LinkRuntime.pairingInProgress.get())
        assertTrue(LinkRuntime.endPairing(second))
    }

    @Test fun `notification permission action never launches sign in`() {
        val host = FakePanelHost(FakeHandle("pairing"), notificationsGranted = false)
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notifications-allow").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notifications-allow").performClick()
        host.completePermissionRequest(granted = false)
        host.completePermissionRequest(granted = true)
        compose.waitForIdle()
        check(host.signInLaunches == 0)
        check(host.serviceStarts == 0)
    }

    @Test fun `successful confirmed unlink shows polite success notice`() {
        val handle = FakeHandle("linked").apply { unlinkAction = { state = "unlinked" } }
        val host = FakePanelHost(handle)
        showPanel(host)
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithTag("google-messages-link-notice")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite))
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_unlinked_done)).fetchSemanticsNode()
    }

    @Test fun `unlink remote failure notice requires the authoritative unlink outcome`() {
        val handle = FakeHandle("linked").apply {
            unlinkAction = {
                state = "unlinked"
                stopCode = "unlink_remote_failed"
            }
        }
        showPanel(FakePanelHost(handle))
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_unlink_remote_failed)).fetchSemanticsNode()
    }

    @Test fun `persist failure hides reconnect and keeps unlink and remove recovery`() {
        val handle = FakeHandle("linked").apply {
            running = false
            stopCode = "persist_failed"
        }
        showPanel(FakePanelHost(handle))
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-unlink").fetchSemanticsNodes().isNotEmpty() }

        check(compose.onAllNodesWithTag("google-messages-link-reconnect").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
    }

    @Test fun `confirmed remove runs once and returns to unlinked`() {
        val handle = FakeHandle("needs_relink")
        val host = FakePanelHost(handle)
        showPanel(host)
        confirm("google-messages-link-remove-local", "google-messages-link-remove-sheet-confirm")
        compose.waitUntil { host.resetCalls == 1 && compose.onAllNodesWithTag("google-messages-link-start").fetchSemanticsNodes().isNotEmpty() }
        check(host.resetCalls == 1)
    }

    @Test fun `throwing reset shows persistence failure and releases the lock`() {
        val host = FakePanelHost(
            handle = FakeHandle("unlinked"),
            openResult = { LinkOpenResult.Unreadable("test") },
            onReset = { throw IllegalStateException("test") },
        )
        showPanel(host)
        confirm("google-messages-link-reset")
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-reset").assertIsEnabled()
        check(host.resetCalls == 1)
    }

    @Test fun `cancelling an active destructive operation releases the lock`() {
        val resetEntered = CountDownLatch(1)
        val releaseReset = CountDownLatch(1)
        val host = FakePanelHost(
            handle = FakeHandle("unlinked"),
            openResult = { LinkOpenResult.Unreadable("test") },
            onReset = {
                resetEntered.countDown()
                awaitIgnoringInterrupts(releaseReset)
            },
        )
        val visible = mutableStateOf(true)
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                if (visible.value) GoogleMessagesLinkPanel(host) {}
            }
        }
        confirm("google-messages-link-reset")
        check(resetEntered.await(5, TimeUnit.SECONDS))

        compose.runOnIdle { visible.value = false }
        releaseReset.countDown()
        compose.waitUntil(5_000) { !LinkOperationLock.inProgress.value }
        assertFalse(LinkOperationLock.inProgress.value)
    }

    @Test fun `unlink failure shows the reloaded authoritative stop code`() {
        val handle = FakeHandle("linked").apply {
            unlinkAction = {
                stopCode = "persist_failed"
                throw IllegalStateException("test")
            }
        }
        val host = FakePanelHost(handle)
        showPanel(host)
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").assertIsEnabled()
    }

    @Test fun `failed relink lifecycle fence prevents native relink and reports persistence failure`() {
        val handle = FakeHandle("needs_relink")
        val host = FakePanelHost(handle).apply { recordPreStartResult = false }
        showPanel(host)

        confirm("google-messages-link-relink")
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        assertEquals(0, handle.relinkCalls)
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
    }

    @Test fun `unlink records unlinked before showing success`() {
        val recordEntered = CountDownLatch(1)
        val releaseRecord = CountDownLatch(1)
        val handle = FakeHandle("linked").apply { unlinkAction = { state = "unlinked" } }
        val host = FakePanelHost(handle).apply {
            onRecordUnlinked = {
                recordEntered.countDown()
                awaitIgnoringInterrupts(releaseRecord)
            }
        }
        showPanel(host)
        confirm("google-messages-link-unlink")
        assertTrue(recordEntered.await(5, TimeUnit.SECONDS))
        check(compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isEmpty())

        releaseRecord.countDown()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_unlinked_done)).fetchSemanticsNode()
    }

    @Test fun `failed unlink lifecycle record reports persistence failure instead of success`() {
        val handle = FakeHandle("linked").apply { unlinkAction = { state = "unlinked" } }
        val host = FakePanelHost(handle).apply { recordUnlinkedResult = false }
        showPanel(host)

        confirm("google-messages-link-unlink")
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
    }

    @Test fun `remote unlink failure records authoritative local deletion and reports remote failure`() {
        val records = AtomicInteger()
        val handle = FakeHandle("linked").apply {
            unlinkAction = {
                state = "unlinked"
                stopCode = "unlink_remote_failed"
                throw IllegalStateException("unlink_remote_failed")
            }
        }
        val host = FakePanelHost(handle).apply { onRecordUnlinked = { records.incrementAndGet() } }
        showPanel(host)

        confirm("google-messages-link-unlink")
        compose.waitUntil { records.get() == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_unlink_remote_failed)).fetchSemanticsNode()
    }

    @Test fun `persist unlink failure without local deletion does not record unlinked`() {
        val records = AtomicInteger()
        val handle = FakeHandle("linked").apply {
            unlinkAction = { throw IllegalStateException("persist_failed") }
        }
        val host = FakePanelHost(handle).apply { onRecordUnlinked = { records.incrementAndGet() } }
        showPanel(host)

        confirm("google-messages-link-unlink")
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        assertEquals(0, records.get())
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
    }

    @Test fun `persist unlink failure with local deletion still records unlinked`() {
        val records = AtomicInteger()
        val handle = FakeHandle("linked").apply {
            unlinkAction = {
                state = "unlinked"
                throw IllegalStateException("persist_failed")
            }
        }
        val host = FakePanelHost(handle).apply { onRecordUnlinked = { records.incrementAndGet() } }
        showPanel(host)

        confirm("google-messages-link-unlink")
        compose.waitUntil { records.get() == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
    }

    @Test fun `recreated panel cannot start a second destructive operation`() {
        val unlinkEntered = CountDownLatch(1)
        val releaseUnlink = CountDownLatch(1)
        val firstHandle = FakeHandle("linked").apply {
            unlinkAction = {
                unlinkEntered.countDown()
                awaitIgnoringInterrupts(releaseUnlink)
                state = "unlinked"
            }
        }
        val firstHost = FakePanelHost(firstHandle)
        val secondHost = FakePanelHost(FakeHandle("needs_relink"))
        val activeHost = mutableStateOf(firstHost)
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                key(activeHost.value) {
                    GoogleMessagesLinkPanel(activeHost.value) {}
                }
            }
        }
        confirm("google-messages-link-unlink")
        check(unlinkEntered.await(5, TimeUnit.SECONDS))

        compose.runOnIdle { activeHost.value = secondHost }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-remove-local").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-remove-local").assertIsNotEnabled()

        releaseUnlink.countDown()
        compose.waitUntil {
            runCatching {
                compose.onNodeWithTag("google-messages-link-remove-local").assertIsEnabled()
            }.isSuccess
        }
    }

    @Test fun `cancelling after pairing acquisition releases pairing ownership`() {
        val pairingAcquired = CountDownLatch(1)
        val releasePairing = CountDownLatch(1)
        val host = FakePanelHost(FakeHandle("needs_relink")).apply {
            onTryBeginPairing = {
                pairingAcquired.countDown()
                awaitIgnoringInterrupts(releasePairing)
            }
        }
        val visible = mutableStateOf(true)
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                if (visible.value) GoogleMessagesLinkPanel(host) {}
            }
        }
        confirm("google-messages-link-relink")
        check(pairingAcquired.await(5, TimeUnit.SECONDS))

        compose.runOnIdle { visible.value = false }
        releasePairing.countDown()
        compose.waitUntil(5_000) { !LinkRuntime.pairingInProgress.get() }
        assertFalse(LinkRuntime.pairingInProgress.get())
    }

    @Test fun `notification actions are disabled during held check and unlink`() {
        val checkEntered = CountDownLatch(1)
        val releaseCheck = CountDownLatch(1)
        val unlinkEntered = CountDownLatch(1)
        val releaseUnlink = CountDownLatch(1)
        val handle = FakeHandle("linked").apply {
            checkAction = { checkEntered.countDown(); awaitIgnoringInterrupts(releaseCheck); 1L }
            unlinkAction = { unlinkEntered.countDown(); awaitIgnoringInterrupts(releaseUnlink) }
        }
        val host = FakePanelHost(handle, notificationsGranted = false)
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-now").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notifications-allow").performClick()
        host.completePermissionRequest(granted = false)
        compose.waitUntil { host.permissionRequests == 1 }
        val application = ApplicationProvider.getApplicationContext<android.app.Application>()
        check(shadowOf(application).nextStartedActivity == null)
        compose.onNodeWithTag("google-messages-link-check-now").performClick()
        compose.waitUntil(5_000) { checkEntered.count == 0L }
        compose.onNodeWithTag("google-messages-link-notifications-allow").assertIsNotEnabled()
        check(host.permissionRequests == 1)
        check(shadowOf(application).nextStartedActivity == null)
        releaseCheck.countDown()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-result").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-unlink").assertIsEnabled()

        confirm("google-messages-link-unlink")
        compose.waitUntil(5_000) { unlinkEntered.count == 0L }
        compose.onNodeWithTag("google-messages-link-notifications-allow").assertIsNotEnabled()
        check(host.permissionRequests == 1)
        check(shadowOf(application).nextStartedActivity == null)
        releaseUnlink.countDown()
    }

    @Test fun `pairing notifications stay disabled while relink is held`() {
        val relinkEntered = CountDownLatch(1)
        val releaseRelink = CountDownLatch(1)
        val handle = FakeHandle("linked").apply {
            relinkAction = { relinkEntered.countDown(); awaitIgnoringInterrupts(releaseRelink) }
        }
        val host = FakePanelHost(handle, notificationsGranted = false)
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notifications-allow").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notifications-allow").performClick()
        host.completePermissionRequest(granted = false)
        compose.waitUntil { host.permissionRequests == 1 }

        handle.state = "needs_relink"
        host.signalChange()
        confirm("google-messages-link-relink")
        compose.waitUntil { host.permissionRequests == 2 }
        host.completePermissionRequest(granted = false)
        compose.waitUntil(5_000) { relinkEntered.count == 0L }
        handle.state = "pairing"
        host.emojiFlow.value = "😀"
        host.signalChange()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-emoji-card").fetchSemanticsNodes().isNotEmpty() }

        val application = ApplicationProvider.getApplicationContext<android.app.Application>()
        compose.onNodeWithTag("google-messages-link-notifications-allow").assertIsNotEnabled()
        check(host.permissionRequests == 2)
        check(shadowOf(application).nextStartedActivity == null)
        releaseRelink.countDown()
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

    private fun resumedOwner(): LifecycleOwner = object : LifecycleOwner {
        override val lifecycle = LifecycleRegistry(this).apply { currentState = Lifecycle.State.RESUMED }
    }

    private fun confirm(actionTag: String, confirmTag: String = "$actionTag-sheet-confirm") {
        compose.waitUntil { compose.onAllNodesWithTag(actionTag).fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag(actionTag).performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil { compose.onAllNodesWithTag(confirmTag).fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag(confirmTag).performSemanticsAction(SemanticsActions.OnClick)
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
        var running = true
        var stopCode = ""
        var relinkCalls = 0
        var unlinkCalls = 0
        var relinkAction: () -> Unit = {}
        var unlinkAction: () -> Unit = {}
        var checkAction: () -> Long = { 1L }
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() { relinkCalls++; relinkAction() }
        override fun run() = Unit
        override fun checkHealth() = checkAction()
        override fun unlink() { unlinkCalls++; unlinkAction() }
        override fun stop() = Unit
        override fun close() = Unit
        override fun status() = statusJson(state, running, stopCode)
    }

    private class FakePanelHost(
        val handle: FakeHandle,
        override var notificationsGranted: Boolean = true,
        private val openResult: () -> LinkOpenResult = { LinkOpenResult.Open(handle) },
        private val onReset: () -> Unit = { handle.state = "unlinked" },
    ) : GoogleMessagesLinkPanelHost {
        override var isChangingConfigurations = false
        private val changesFlow = MutableStateFlow(0L)
        override val changes: StateFlow<Long> = changesFlow
        val emojiFlow = MutableStateFlow<String?>(null)
        override val emoji: StateFlow<String?> = emojiFlow
        private val permissionResultFlow = MutableStateFlow(0L)
        override val permissionResults: StateFlow<Long> = permissionResultFlow
        var permissionRequests = 0
        var signInLaunches = 0
        var serviceStarts = 0
        var resetCalls = 0
        val pairingTokens = mutableListOf<Long>()
        val endedTokens = mutableListOf<Long>()
        var onTryBeginPairing: () -> Unit = {}
        var recordPreStartResult = true
        var recordUnlinkedResult = true
        var onRecordUnlinked: () -> Unit = {}
        val relinkCalls get() = handle.relinkCalls

        override fun open(): LinkOpenResult = openResult()
        override fun recordPreStart(): Boolean = recordPreStartResult
        override fun recordUnlinked(): Boolean {
            onRecordUnlinked()
            return recordUnlinkedResult
        }
        override fun resetLocalState() { resetCalls++; onReset() }
        override fun pairingInProgress(): Boolean = LinkRuntime.pairingInProgress.get()
        override fun tryBeginPairing(): Long? = LinkRuntime.tryBeginPairing()?.also {
            pairingTokens += it
            onTryBeginPairing()
        }
        override fun endPairing(token: Long) { endedTokens += token; LinkRuntime.endPairing(token) }
        override fun requestNotificationPermission() { permissionRequests++ }
        override fun launchSignIn(onResult: (String?) -> Unit) { signInLaunches++ }
        override fun startService() { serviceStarts++ }

        fun completePermissionRequest(granted: Boolean = notificationsGranted) {
            notificationsGranted = granted
            permissionResultFlow.value++
        }
        fun signalChange() { changesFlow.value++ }
    }

    private fun blockingHandle(
        state: String,
        entered: CountDownLatch,
        release: CountDownLatch,
        completed: CountDownLatch,
    ) = object : FakeHandle(state) {
        override fun status(): String {
            entered.countDown()
            while (true) {
                try {
                    if (release.await(20, TimeUnit.MILLISECONDS)) {
                        return super.status().also { completed.countDown() }
                    }
                } catch (_: InterruptedException) {
                    // Simulates a native call that does not cooperate with coroutine cancellation.
                }
            }
        }
    }

    private fun awaitIgnoringInterrupts(release: CountDownLatch) {
        while (true) {
            try {
                if (release.await(20, TimeUnit.MILLISECONDS)) return
            } catch (_: InterruptedException) {
                // Models cancellation-insensitive native work.
            }
        }
    }

    private companion object {
        fun statusJson(state: String, running: Boolean = true, stopCode: String = "") =
            """{"seq":1,"state":"$state","running":$running,"healthy":true,"phone_responding":true,"last_ok_ms":0,"stop_code":"$stopCode"}"""
    }

    private class InMemoryStore : LinkStateStorage {
        override fun read() = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }
}
