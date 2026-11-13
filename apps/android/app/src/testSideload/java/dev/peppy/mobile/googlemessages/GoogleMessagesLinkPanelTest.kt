package dev.peppy.mobile.googlemessages

import android.content.ActivityNotFoundException
import android.content.ContextWrapper
import android.content.Intent
import android.os.Bundle
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanel
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanelDetails
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

/** Which composable surface a test renders: the card, the details page, or a card that opens details on Manage. */
private enum class PanelSurface { Card, Details, Managed }

private class GoogleMessagesGuideContext(base: android.content.Context) : ContextWrapper(base) {
    var rejectActivityLaunch = true
    var startedIntent: Intent? = null

    override fun startActivity(intent: Intent) {
        if (rejectActivityLaunch) throw ActivityNotFoundException()
        startedIntent = intent
    }

    override fun startActivity(intent: Intent, options: Bundle?) = startActivity(intent)
}

@Composable
private fun LinkPanelSurface(surface: PanelSurface, host: GoogleMessagesLinkPanelHost?) {
    when (surface) {
        PanelSurface.Card -> if (host == null) GoogleMessagesLinkPanel {} else GoogleMessagesLinkPanel(host, onChanged = {})
        PanelSurface.Details -> if (host == null) {
            GoogleMessagesLinkPanelDetails(
                onBack = {}, onChanged = {}, notice = null, onNoticeChange = {},
                dismissedSeq = null, onDismissedSeqChange = {}, notificationAsked = false, onNotificationAskedChange = {},
            )
        } else {
            GoogleMessagesLinkPanelDetails(host, onChanged = {})
        }
        PanelSurface.Managed -> ManagedLinkPanel(host)
    }
}

/** Mirrors SmsTab: the card and details share hoisted notice and per-link UI memory, and only one is shown at a time. */
@Composable
private fun ManagedLinkPanel(host: GoogleMessagesLinkPanelHost?) {
    val inDetails = remember { mutableStateOf(false) }
    val notice = remember { mutableStateOf<String?>(null) }
    val dismissedSeq = remember { mutableStateOf<Long?>(null) }
    val notificationAsked = remember { mutableStateOf(false) }
    val onNoticeChange: (String?) -> Unit = { notice.value = it }
    val onDismissedSeqChange: (Long?) -> Unit = { dismissedSeq.value = it }
    val onNotificationAskedChange: (Boolean) -> Unit = { notificationAsked.value = it }
    if (inDetails.value) {
        val onBack: () -> Unit = { inDetails.value = false }
        if (host == null) {
            GoogleMessagesLinkPanelDetails(
                onBack = onBack, onChanged = {}, notice = notice.value, onNoticeChange = onNoticeChange,
                dismissedSeq = dismissedSeq.value, onDismissedSeqChange = onDismissedSeqChange,
                notificationAsked = notificationAsked.value, onNotificationAskedChange = onNotificationAskedChange,
            )
        } else {
            GoogleMessagesLinkPanelDetails(
                host, onBack = onBack, onChanged = {}, notice = notice.value, onNoticeChange = onNoticeChange,
                dismissedSeq = dismissedSeq.value, onDismissedSeqChange = onDismissedSeqChange,
                notificationAsked = notificationAsked.value, onNotificationAskedChange = onNotificationAskedChange,
            )
        }
    } else {
        val onManage: () -> Unit = { inDetails.value = true }
        if (host == null) {
            GoogleMessagesLinkPanel(
                onChanged = {}, notice = notice.value, onNoticeChange = onNoticeChange, onManage = onManage,
                dismissedSeq = dismissedSeq.value, onDismissedSeqChange = onDismissedSeqChange,
                notificationAsked = notificationAsked.value, onNotificationAskedChange = onNotificationAskedChange,
            )
        } else {
            GoogleMessagesLinkPanel(
                host, onChanged = {}, onManage = onManage, notice = notice.value, onNoticeChange = onNoticeChange,
                dismissedSeq = dismissedSeq.value, onDismissedSeqChange = onDismissedSeqChange,
                notificationAsked = notificationAsked.value, onNotificationAskedChange = onNotificationAskedChange,
            )
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkPanelTest {
    // Panel effects resume on the main looper, as in production; the default unconfined test
    // dispatcher would resume them on the Dispatchers.IO worker that finished the host call.
    private val compose = createComposeRule(Dispatchers.Main)
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @After fun tearDown() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        LinkCoreBridge.resetForTest()
        GoogleMessagesLinkTestHooks.available = null
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        GoogleMessagesLink.clearCutoverMayHaveMissedMessages(context)
    }

    @Test fun `unavailable engine renders nothing`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = false
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("unavailable")
        }
        GoogleMessagesLinkTestHooks.available = false
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        check(compose.onAllNodesWithTag("google-messages-link-card").fetchSemanticsNodes().isEmpty())
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

    @Test fun `linked card shows a success chip and the manage row`() = panel("linked") {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_status_linked)).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-manage").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-check-now").fetchSemanticsNodes().isEmpty())
        check(compose.onAllNodesWithTag("google-messages-link-unlink").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `unlinked card with the real unlinked status stays neutral`() = panel("unlinked", statusJson = UNLINKED_JSON) {
        compose.onNodeWithTag("google-messages-link-card-tone-neutral").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-card-tone-warning").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `pairing card with the real unlinked-style running flags stays neutral`() = panel("pairing", statusJson = PAIRING_JSON) {
        compose.onNodeWithTag("google-messages-link-pairing").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-card-tone-neutral").fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-card-tone-warning").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `linked details present health, typing, battery and unlink actions`() = panel("linked", PanelSurface.Details) {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithTag("google-messages-link-status").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-chip").fetchSemanticsNode()
        check(compose.onAllNodesWithText(context.getString(R.string.peppy_google_messages_link_connection_connected)).fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("google-messages-link-check-now").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-battery-row").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-typing-row").fetchSemanticsNode()
    }

    @Test fun `healthy card shows the last answer and no connection row`() = panel("linked") {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithTag("google-messages-link-last-ok").fetchSemanticsNode()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_last_ok_label)).fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-connection").fetchSemanticsNodes().isEmpty())
        check(compose.onAllNodesWithText(context.getString(R.string.peppy_google_messages_link_connection_connected)).fetchSemanticsNodes().isEmpty())
    }

    @Test fun `degraded card with the link not running keeps help, last answer, reconnect and manage`() =
        degradedCard(FakeHandle("linked").apply { running = false }, R.string.peppy_google_messages_link_help_not_running, "google-messages-link-reconnect")

    @Test fun `degraded card with the phone not responding keeps help, last answer, check now and manage`() =
        degradedCard(FakeHandle("linked").apply { phoneResponding = false }, R.string.peppy_google_messages_link_help_phone_not_responding, "google-messages-link-check-now")

    @Test fun `degraded card with an unconfirmed link keeps help, last answer, check now and manage`() =
        degradedCard(FakeHandle("linked").apply { healthy = false }, R.string.peppy_google_messages_link_help_unconfirmed, "google-messages-link-check-now")

    @Test fun `linked not running card offers reconnect and manage`() {
        showPanel(FakePanelHost(FakeHandle("linked").apply { running = false }), readinessTag = "google-messages-link-chip")
        compose.onNodeWithTag("google-messages-link-reconnect").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-card-tone-warning").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-manage").fetchSemanticsNode()
    }

    @Test fun `pairing emoji stays on the card`() {
        val host = FakePanelHost(FakeHandle("pairing")).apply { emojiFlow.value = "😀" }
        showPanel(host, readinessTag = "google-messages-link-emoji-card")
        compose.onNodeWithTag("google-messages-link-emoji-card").assertTextContains("😀", substring = true)
    }

    @Test fun `cutover ambiguity notice shows on the card and dismisses`() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        LinkCoreBridgeTestHooks.cutoverAlertIds = { setOf("conversation") }
        panel("linked") {
            compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice-cutover").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_cutover_ambiguous)).fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-notice-cutover-dismiss").performClick()
            compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice-cutover").fetchSemanticsNodes().isEmpty() }
        }
        check(!GoogleMessagesLink.cutoverMayHaveMissedMessages(context))
    }

    @Test fun `dismissing a local notice does not record the stop code as dismissed`() {
        val host = FakePanelHost(FakeHandle("linked").apply { stopCode = "persist_failed" })
        val dismissedChanges = mutableListOf<Long?>()
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                GoogleMessagesLinkPanel(host, onChanged = {}, notice = "unlinked_done", onDismissedSeqChange = { dismissedChanges += it })
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice-dismiss").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notice-dismiss").performClick()
        assertTrue(dismissedChanges.isEmpty())
    }

    @Test fun `dismissing the stop-code notice records its status sequence as dismissed`() {
        val host = FakePanelHost(FakeHandle("linked").apply { stopCode = "persist_failed" })
        val dismissedChanges = mutableListOf<Long?>()
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                GoogleMessagesLinkPanel(host, onChanged = {}, onDismissedSeqChange = { dismissedChanges += it })
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice-dismiss").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-notice-dismiss").performClick()
        assertEquals(listOf<Long?>(1L), dismissedChanges)
    }

    @Test fun `linked typing sharing defaults on and writes each toggle once`() {
        val host = FakePanelHost(FakeHandle("linked"))
        showPanel(host, PanelSurface.Details)
        awaitTypingEnabled()
        compose.onNodeWithTag("google-messages-link-typing-row").performScrollTo().performClick()
        compose.waitUntil { host.typingWrites == listOf(false) }
        compose.onNodeWithTag("google-messages-link-typing-row").performScrollTo().performClick()
        compose.waitUntil { host.typingWrites == listOf(false, true) }
    }

    @Test fun `typing sharing failure keeps the last confirmed value and shows the error`() {
        val host = FakePanelHost(FakeHandle("linked")).apply { typingWriteSucceeds = false }
        showPanel(host, PanelSurface.Details)
        awaitTypingEnabled()
        compose.onNodeWithTag("google-messages-link-typing-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-typing-error").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-typing-row").assert(SemanticsMatcher.expectValue(SemanticsProperties.ToggleableState, androidx.compose.ui.state.ToggleableState.On))
    }

    @Test fun `typing sharing toggle has no info action`() {
        val host = FakePanelHost(FakeHandle("linked"))
        showPanel(host, PanelSurface.Details)
        awaitTypingEnabled()
        compose.onNodeWithTag("google-messages-link-typing-row").performScrollTo().assertIsDisplayed()
        assertTrue(compose.onAllNodesWithTag("info-typing-share").fetchSemanticsNodes().isEmpty())
    }

    @Test fun `repair state offers re-link on the card and unlink and remove in details`() {
        panel("needs_relink", PanelSurface.Managed) {
            compose.onNodeWithTag("google-messages-link-relink").fetchSemanticsNode()
            openManage()
            compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
        }
    }

    @Test fun `unlink and remove require confirmation`() {
        panel("needs_relink", PanelSurface.Managed) {
            openManage()
            compose.onNodeWithTag("google-messages-link-unlink").performClick()
            compose.onNodeWithTag("google-messages-link-unlink-sheet").fetchSemanticsNode()
        }
    }

    @Test fun `health check reports a completed result`() = panel("linked", PanelSurface.Details) {
        compose.onNodeWithTag("google-messages-link-check-now").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-result").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun `signed out state never offers unlink`() = panel("signed_out", PanelSurface.Managed) {
        compose.onNodeWithTag("google-messages-link-sign-in-again").fetchSemanticsNode()
        openManage()
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
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_status_unreadable)).fetchSemanticsNode()
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
                PeppyTheme { GoogleMessagesLinkPanel(host, onChanged = {}) }
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
                PeppyTheme { GoogleMessagesLinkPanel(host, onChanged = {}) }
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
                PeppyTheme { GoogleMessagesLinkPanel(host, onChanged = {}) }
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
        showPanel(host, PanelSurface.Managed)
        openManage()
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithTag("google-messages-link-notice")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite))
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_unlinked_done)).fetchSemanticsNode()
        check(host.ownerUnlinkEvents == listOf("unlinked", "completed"))
    }

    @Test fun `unlink remote failure notice requires the authoritative unlink outcome`() {
        val handle = FakeHandle("linked").apply {
            unlinkAction = {
                state = "unlinked"
                stopCode = "unlink_remote_failed"
            }
        }
        showPanel(FakePanelHost(handle), PanelSurface.Managed)
        openManage()
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
        showPanel(FakePanelHost(handle), PanelSurface.Managed, readinessTag = "google-messages-link-chip")

        check(compose.onAllNodesWithTag("google-messages-link-reconnect").fetchSemanticsNodes().isEmpty())
        openManage()
        check(compose.onAllNodesWithTag("google-messages-link-reconnect").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
    }

    @Test fun `confirmed remove runs once and returns to unlinked`() {
        val handle = FakeHandle("needs_relink")
        val host = FakePanelHost(handle)
        showPanel(host, PanelSurface.Managed)
        openManage()
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
                if (visible.value) GoogleMessagesLinkPanel(host, onChanged = {})
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
        showPanel(host, PanelSurface.Managed)
        openManage()
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_persist_failed)).fetchSemanticsNode()
        // Still linked: the operation stays on details, so unlink is enabled again there.
        compose.onNodeWithTag("google-messages-details-screen").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").assertIsEnabled()
    }

    @Test fun `status flip to unlinked keeps details open until the lock is released and the notice is set`() {
        val recordEntered = CountDownLatch(1)
        val releaseRecord = CountDownLatch(1)
        val handle = FakeHandle("linked").apply { unlinkAction = { state = "unlinked" } }
        val host = FakePanelHost(handle).apply {
            onRecordUnlinked = {
                recordEntered.countDown()
                awaitIgnoringInterrupts(releaseRecord)
            }
        }
        showPanel(host, PanelSurface.Managed)
        openManage()
        confirm("google-messages-link-unlink")
        assertTrue(recordEntered.await(5, TimeUnit.SECONDS))
        // The reload already reports unlinked, but details must not close under the held operation.
        compose.waitUntil(5_000) { host.observedStates.contains("unlinked") }
        compose.onNodeWithTag("google-messages-details-screen").fetchSemanticsNode()
        assertTrue(LinkOperationLock.inProgress.value)

        releaseRecord.countDown()
        compose.waitUntil(5_000) { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }
        compose.waitUntil(5_000) { !LinkOperationLock.inProgress.value }
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_unlinked_done)).fetchSemanticsNode()
        assertEquals(listOf("unlinked", "completed"), host.ownerUnlinkEvents)
    }

    @Test fun `unlink that leaves the link linked keeps details open with a retry notice`() {
        val handle = FakeHandle("linked").apply { unlinkAction = { throw IllegalStateException("test") } }
        showPanel(FakePanelHost(handle), PanelSurface.Managed)
        openManage()
        confirm("google-messages-link-unlink")
        compose.waitUntil { handle.unlinkCalls == 1 && compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isNotEmpty() }

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_notice_unlink_failed)).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-details-screen").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").assertIsEnabled()
    }

    @Test fun `reset after relink admission prevents prestart service and native relink`() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val acquireEntered = CountDownLatch(1)
        val releaseAcquire = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        val handle = FakeHandle("needs_relink")
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        val host = FakePanelHost(handle).apply {
            acquirePairingLease = { token ->
                LinkRuntime.acquirePairingLease(context, token).also {
                    acquireEntered.countDown()
                    awaitIgnoringInterrupts(releaseAcquire)
                }
            }
            recordTokenPreStart = { token -> LinkRuntime.recordPreStart(context, token) }
        }
        showPanel(host)

        confirm("google-messages-link-relink")
        assertTrue(acquireEntered.await(5, TimeUnit.SECONDS))
        LinkRuntime.resetLocalState(context)
        releaseAcquire.countDown()
        compose.waitUntil(5_000) { host.endedTokens.isNotEmpty() }

        assertEquals(listOf("unlinked"), synchronized(recorded) { recorded.toList() })
        assertEquals(0, host.serviceStarts)
        assertEquals(0, handle.relinkCalls)
    }

    @Test fun `reset while admitted prestart records prevents service and retired relink`() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val preStartEntered = CountDownLatch(1)
        val releasePreStart = CountDownLatch(1)
        val handleClosed = CountDownLatch(1)
        val resetFinished = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        val handle = object : FakeHandle("needs_relink") {
            override fun close() { handleClosed.countDown() }
        }
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkCoreBridge.recorder = { _, status ->
            if (status.state == "unknown") {
                preStartEntered.countDown()
                awaitIgnoringInterrupts(releasePreStart)
            }
            synchronized(recorded) { recorded += status.state }
            true
        }
        val host = FakePanelHost(handle).apply {
            acquirePairingLease = { token -> LinkRuntime.acquirePairingLease(context, token) }
            recordTokenPreStart = { token -> LinkRuntime.recordPreStart(context, token) }
        }
        showPanel(host)

        confirm("google-messages-link-relink")
        assertTrue(preStartEntered.await(5, TimeUnit.SECONDS))
        Thread {
            LinkRuntime.resetLocalState(context)
            resetFinished.countDown()
        }.start()
        assertTrue(handleClosed.await(5, TimeUnit.SECONDS))
        releasePreStart.countDown()
        assertTrue(resetFinished.await(5, TimeUnit.SECONDS))
        compose.waitUntil(5_000) { host.endedTokens.isNotEmpty() }

        assertEquals(listOf("unknown", "unlinked"), synchronized(recorded) { recorded.toList() })
        assertEquals(0, host.serviceStarts)
        assertEquals(0, handle.relinkCalls)
    }

    @Test fun `reset between displayed handle and admission never relinks the retired handle`() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val retired = FakeHandle("needs_relink")
        val replacement = FakeHandle("unlinked")
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = replacement
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        LinkCoreBridge.recorder = { _, _ -> true }
        val host = FakePanelHost(retired).apply {
            acquirePairingLease = { token -> LinkRuntime.acquirePairingLease(context, token) }
            recordTokenPreStart = { token -> LinkRuntime.recordPreStart(context, token) }
        }
        showPanel(host)
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-relink").fetchSemanticsNodes().isNotEmpty() }
        LinkRuntime.resetLocalState(context)

        confirm("google-messages-link-relink")
        compose.waitUntil(5_000) { host.endedTokens.isNotEmpty() }

        assertEquals(0, retired.relinkCalls)
    }

    @Test fun `owner unlink fences a linked status observation delayed after its read`() {
        val observationEntered = CountDownLatch(1)
        val releaseObservation = CountDownLatch(1)
        val observationCalls = AtomicInteger()
        val handle = FakeHandle("linked").apply { unlinkAction = { state = "unlinked" } }
        val host = FakePanelHost(handle).apply {
            onRecordStatusObservation = {
                if (observationCalls.incrementAndGet() == 1) {
                    observationEntered.countDown()
                    awaitIgnoringInterrupts(releaseObservation)
                }
            }
        }
        showPanel(host, PanelSurface.Managed, readinessTag = null)
        assertTrue(observationEntered.await(5, TimeUnit.SECONDS))

        openManage()
        confirm("google-messages-link-unlink")
        compose.waitUntil(5_000) { handle.unlinkCalls == 1 && host.observedStates.contains("unlinked") }
        releaseObservation.countDown()
        compose.waitUntil(5_000) { observationCalls.get() > 1 }

        assertEquals("unlinked", host.observedStates.last())
    }

    @Test fun `opening a sealed terminal link reconciles core without starting native work`() {
        val handle = object : FakeHandle("needs_relink") {
            var pairCalls = 0
            var runCalls = 0
            override fun pair(cookiesJson: ByteArray) { pairCalls++ }
            override fun run() { runCalls++ }
        }
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        GoogleMessagesLinkTestHooks.available = true
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = handle
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }

        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        compose.waitUntil(5_000) { synchronized(recorded) { recorded.contains("needs_relink") } }

        assertEquals(listOf("needs_relink"), synchronized(recorded) { recorded.toList() })
        assertEquals(0, handle.pairCalls)
        assertEquals(0, handle.runCalls)
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
        showPanel(host, PanelSurface.Managed)
        openManage()
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
        showPanel(host, PanelSurface.Managed)
        openManage()

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
        showPanel(host, PanelSurface.Managed)
        openManage()

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
        showPanel(host, PanelSurface.Managed)
        openManage()

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
        showPanel(host, PanelSurface.Managed)
        openManage()

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
                    GoogleMessagesLinkPanelDetails(activeHost.value, onBack = {}, onChanged = {})
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
                if (visible.value) GoogleMessagesLinkPanel(host, onChanged = {})
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
        showPanel(host, PanelSurface.Details, readinessTag = "google-messages-link-check-now")
        compose.onNodeWithTag("google-messages-link-notifications-allow").performScrollTo().performClick()
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
        val handle = FakeHandle("needs_relink").apply {
            relinkAction = { relinkEntered.countDown(); awaitIgnoringInterrupts(releaseRelink) }
        }
        val host = FakePanelHost(handle, notificationsGranted = false)
        showPanel(host)
        confirm("google-messages-link-relink")
        compose.waitUntil { host.permissionRequests == 1 }
        host.completePermissionRequest(granted = false)
        compose.waitUntil(5_000) { relinkEntered.count == 0L }
        handle.state = "pairing"
        host.emojiFlow.value = "😀"
        host.signalChange()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-emoji-card").fetchSemanticsNodes().isNotEmpty() }

        val application = ApplicationProvider.getApplicationContext<android.app.Application>()
        compose.onNodeWithTag("google-messages-link-notifications-allow").assertIsNotEnabled()
        check(host.permissionRequests == 1)
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
                    PeppyTheme { GoogleMessagesLinkPanelDetails(host, onChanged = {}) }
                }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-check-now").fetchSemanticsNodes().isNotEmpty() }
        val polite = SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite)
        // The status row is a plain label/value pair; the value carries the text, the row carries the tag.
        compose.onNodeWithTag("google-messages-link-status").fetchSemanticsNode()
        compose.onNodeWithText(ApplicationProvider.getApplicationContext<android.content.Context>().getString(R.string.peppy_google_messages_link_status_linked)).fetchSemanticsNode()
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

    @Test
    @Config(sdk = [35], qualifiers = "w412dp-h1600dp")
    fun `details guide opens its exact URL and announces a retryable failure`() {
        val host = FakePanelHost(FakeHandle("linked"), notificationsGranted = true)
        GoogleMessagesLinkTestHooks.available = true
        lateinit var context: GoogleMessagesGuideContext
        compose.setContent {
            val baseContext = LocalContext.current
            context = remember(baseContext) { GoogleMessagesGuideContext(baseContext) }
            CompositionLocalProvider(LocalContext provides context) {
                PeppyTheme { GoogleMessagesLinkPanelDetails(host, onChanged = {}) }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-details-guide").fetchSemanticsNodes().isNotEmpty() }

        compose.onNodeWithTag("google-messages-details-guide").performClick()

        compose.onNodeWithTag("google-messages-details-guide-error")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite))
        context.rejectActivityLaunch = false
        compose.onNodeWithTag("google-messages-details-guide").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-details-guide-error").fetchSemanticsNodes().isEmpty() }
        val intent = requireNotNull(context.startedIntent)
        assertEquals(Intent.ACTION_VIEW, intent.action)
        assertEquals("https://peppy.pro/docs/google-messages", intent.data.toString())
    }

    @Test fun `narrow large-text card keeps re-link and manage reachable`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("needs_relink")))
        assertReachable("google-messages-link-relink")
        assertReachable("google-messages-link-manage")
    }

    @Test fun `narrow large-text card keeps sign in again and manage reachable`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("signed_out")))
        assertReachable("google-messages-link-sign-in-again")
        assertReachable("google-messages-link-manage")
    }

    @Test fun `narrow large-text card keeps reconnect and manage reachable`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("linked").apply { running = false }))
        assertReachable("google-messages-link-reconnect")
        assertReachable("google-messages-link-manage")
    }

    @Test fun `action row sits side by side at normal text in a wide enough card`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("needs_relink")), fontScale = 1f, containerWidth = 360.dp)
        assertEquals(boundsTop("google-messages-link-relink"), boundsTop("google-messages-link-manage"))
    }

    @Test fun `action row stacks at normal text when the card is under 260 dp wide`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("needs_relink")), fontScale = 1f, containerWidth = 280.dp)
        assertTrue(boundsTop("google-messages-link-manage") > boundsTop("google-messages-link-relink"))
    }

    @Test fun `notice dismiss stays reachable and dismisses at large text`() {
        showNarrowLargeTextCard(FakePanelHost(FakeHandle("linked")), notice = "unlinked_done")
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice-dismiss").fetchSemanticsNodes().isNotEmpty() }
        assertReachable("google-messages-link-notice-dismiss")
        compose.onNodeWithTag("google-messages-link-notice-dismiss").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-notice").fetchSemanticsNodes().isEmpty() }
    }

    @Test fun `details needs re-link status line shows the chip without a last answer`() = panel("needs_relink", PanelSurface.Details) {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithTag("google-messages-link-status").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-chip").fetchSemanticsNode()
        compose.onNodeWithText(context.getString(R.string.peppy_google_messages_link_status_needs_relink)).fetchSemanticsNode()
        check(compose.onAllNodesWithTag("google-messages-link-last-ok").fetchSemanticsNodes().isEmpty())
    }

    /** Degraded linked card: the help line, last answer, the primary action and Manage are all present. */
    private fun degradedCard(handle: FakeHandle, helpText: Int, primaryTag: String) {
        showPanel(FakePanelHost(handle), readinessTag = "google-messages-link-chip")
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.onNodeWithText(context.getString(helpText)).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-last-ok").fetchSemanticsNode()
        compose.onNodeWithTag(primaryTag).fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-manage").fetchSemanticsNode()
    }

    /** Renders the card in a narrow container. The defaults are 2x font scale in 280 dp, the narrowest width the card supports. */
    private fun showNarrowLargeTextCard(host: FakePanelHost, notice: String? = null, fontScale: Float = 2f, containerWidth: Dp = 280.dp) {
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                PeppyTheme {
                    Column(Modifier.width(containerWidth).height(480.dp).verticalScroll(rememberScrollState())) {
                        GoogleMessagesLinkPanel(host, onChanged = {}, onManage = {}, notice = notice)
                    }
                }
            }
        }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-chip").fetchSemanticsNodes().isNotEmpty() }
    }

    private fun boundsTop(tag: String) = compose.onNodeWithTag(tag).getBoundsInRoot().top

    /** Scrolls to a control in the narrow container and checks it is still a full touch target. */
    private fun assertReachable(tag: String) {
        compose.onNodeWithTag(tag).performScrollTo().assertIsDisplayed().assertHeightIsAtLeast(48.dp)
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

    /** The typing toggle is disabled until the confirmed value loads from preferences. */
    private fun awaitTypingEnabled() {
        compose.waitUntil { runCatching { compose.onNodeWithTag("google-messages-link-typing-row").assertIsEnabled() }.isSuccess }
    }

    /** Opens the details page from the card's Manage row and waits for its status to load. */
    private fun openManage() {
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-manage").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("google-messages-link-manage").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-status").fetchSemanticsNodes().isNotEmpty() }
    }

    private fun panel(
        state: String,
        surface: PanelSurface = PanelSurface.Card,
        statusJson: String = """{"seq":1,"state":"$state","running":true,"healthy":true,"phone_responding":true,"last_ok_ms":0,"stop_code":""}""",
        assertion: () -> Unit,
    ) {
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
                override fun status() = statusJson
            }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        compose.setContent { PeppyTheme { LinkPanelSurface(surface, null) } }
        val readinessTag = if (surface == PanelSurface.Details) "google-messages-link-status" else "google-messages-link-chip"
        compose.waitUntil { compose.onAllNodesWithTag(readinessTag).fetchSemanticsNodes().isNotEmpty() }
        assertion()
    }

    private fun showPanel(host: FakePanelHost, surface: PanelSurface = PanelSurface.Card, readinessTag: String? = null) {
        GoogleMessagesLinkTestHooks.available = true
        compose.setContent {
            PeppyTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    LinkPanelSurface(surface, host)
                }
            }
        }
        readinessTag?.let { tag ->
            compose.waitUntil { compose.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty() }
        }
    }

    private open class FakeHandle(var state: String) : LinkHandle {
        var running = true
        var phoneResponding = true
        var healthy = true
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
        override fun status() = statusJson(state, running, stopCode, phoneResponding, healthy)
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
        var acquirePairingLease: ((Long) -> LinkHandleLease?)? = null
        var recordTokenPreStart: ((Long) -> Boolean)? = null
        var onRecordStatusObservation: () -> Unit = {}
        private var observationGeneration = 0L
        val observedStates = mutableListOf<String>()
        var recordPreStartResult = true
        var recordUnlinkedResult = true
        var typingSharingValue = true
        var typingWriteSucceeds = true
        val typingWrites = mutableListOf<Boolean>()
        var onRecordUnlinked: () -> Unit = {}
        val ownerUnlinkEvents = mutableListOf<String>()
        val relinkCalls get() = handle.relinkCalls

        override fun open(): LinkOpenResult = openResult()
        override fun recordPreStart(): Boolean = recordPreStartResult
        override fun recordPreStart(token: Long): Boolean = recordTokenPreStart?.invoke(token) ?: recordPreStartResult
        override fun acquirePairingLease(token: Long): LinkHandleLease? =
            acquirePairingLease?.invoke(token) ?: LinkHandleLease(handle, 0)
        override fun captureStatusObservation(handle: LinkHandle): LinkObservation =
            LinkObservation(synchronized(this) { observationGeneration }, LinkHandleLease(handle, 0))
        override fun recordStatusObservation(observation: LinkObservation, status: LinkStatus): Boolean {
            onRecordStatusObservation()
            synchronized(this) {
                if (observation.token == observationGeneration) observedStates += status.state
            }
            return true
        }
        override fun beginOwnerUnlink() = synchronized(this) { observationGeneration += 1 }
        override fun recordUnlinked(): Boolean {
            onRecordUnlinked()
            if (recordUnlinkedResult) synchronized(this) {
                observedStates += "unlinked"
                ownerUnlinkEvents += "unlinked"
            }
            return recordUnlinkedResult
        }
        override fun recordExplicitUnlinkCompleted(): Boolean = synchronized(this) {
            ownerUnlinkEvents += "completed"
            true
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
        override fun typingSharing() = typingSharingValue
        override fun setTypingSharing(enabled: Boolean): Boolean {
            typingWrites += enabled
            if (typingWriteSucceeds) typingSharingValue = enabled
            return typingWriteSucceeds
        }

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
        // Same payloads as GoogleMessagesLinkServiceTest: unlinked and pairing report running and phone_responding as false.
        const val UNLINKED_JSON = """{"seq":1,"state":"unlinked","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val PAIRING_JSON = """{"seq":1,"state":"pairing","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""

        fun statusJson(state: String, running: Boolean = true, stopCode: String = "", phoneResponding: Boolean = true, healthy: Boolean = true) =
            """{"seq":1,"state":"$state","running":$running,"healthy":$healthy,"phone_responding":$phoneResponding,"last_ok_ms":0,"stop_code":"$stopCode"}"""
    }

    private class InMemoryStore : LinkStateStorage {
        override fun read() = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }
}
