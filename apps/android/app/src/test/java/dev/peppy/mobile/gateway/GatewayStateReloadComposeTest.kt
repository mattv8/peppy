package dev.peppy.mobile.gateway

import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import dev.peppy.mobile.testing.ComposeTestActivityRule
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The SMS screen's status path end to end: persisted changes reach the mounted card, the too-large state and the
 * Finished notice without any manual refresh, and none of it enqueues gateway work. The clock is manual, so the
 * 6-second notice is exact; waits pump one frame at a time.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayStateReloadComposeTest : GatewayTestBase() {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    private var reloads = 0

    @Test
    fun persistedProgressTooLargeAndFinishReachTheMountedScreenWithoutRefresh() {
        enrollAndUnlock()
        val store = GatewayStateStore(context)
        store.setDownloading(42_000uL, 108_882uL)
        scheduled.clear()
        SetupFinishTracker.resetForTest()
        compose.mainClock.autoAdvance = false
        compose.setContent { MaterialTheme { StatusHost(context) { reloads++ } } }
        pumpUntil { textShown(count(42_000, 108_882)) }
        awaitWatcher(store)

        store.setDownloading(50_000uL, 108_882uL)
        pumpUntil { textShown(count(50_000, 108_882)) }

        store.advance(SyncPhase.LIVE)
        pumpUntil { textShown("Other devices can now send through this phone.") }

        store.recordHistoryTooLarge(nowMs = 1_000L, kind = HistoryTooLargeKind.DOWNLOAD_BYTE_CAP)
        pumpUntil { tagShown("host-too-large") }
        assertTrue("the too-large state hides the card", compose.onAllNodesWithTag("sync-setup-card", useUnmergedTree = true).fetchSemanticsNodes().isEmpty())

        assertTrue("the status reads must not enqueue any gateway work", scheduled.isEmpty())
        assertNull(GatewayStateStore(context).setupProgress)
    }

    /** Mirrors the SMS screen wiring in MainActivity: the same effects, no refresh button. */
    @Composable
    private fun StatusHost(context: Context, onReload: () -> Unit) {
        var status by remember { mutableStateOf<GatewayStatus?>(null) }
        LaunchedEffect(Unit) { status = withContext(Dispatchers.IO) { NativeGateway.status(context) } }
        GatewayStateReloadEffect(context) {
            onReload()
            status = withContext(Dispatchers.IO) { NativeGateway.status(context) }
        }
        LaunchedEffect(status) { SetupFinishTracker.observe(status) }
        SetupProgressCard(status)
        if (historyTooLargeRibbonVisible(status)) Text("too large", Modifier.testTag("host-too-large"))
    }

    /**
     * The watcher opens its preferences on the IO thread and pauses on the real main looper, so frames are pumped
     * until the condition holds. Each pump advances one frame of the manual clock and yields briefly.
     */
    private fun pumpUntil(condition: () -> Boolean) {
        repeat(PUMP_LIMIT) {
            if (condition()) return
            compose.mainClock.advanceTimeByFrame()
            compose.waitForIdle()
            Thread.sleep(PUMP_SLEEP_MS)
        }
        fail("condition not reached after $PUMP_LIMIT pumps")
    }

    /**
     * Writes until one produces a reload, which proves the listener is registered, then lets the reload pause end.
     */
    private fun awaitWatcher(store: GatewayStateStore) {
        val before = reloads
        pumpUntil {
            store.setChecking()
            reloads > before
        }
        repeat(PAUSE_FRAMES) {
            compose.mainClock.advanceTimeByFrame()
            compose.waitForIdle()
        }
    }

    private fun count(staged: Long, expected: Long) =
        formatSetupCount(staged) + " of " + formatSetupCount(expected) + " items"

    private fun textShown(text: String) =
        compose.onAllNodesWithText(text, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun tagShown(tag: String) =
        compose.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private companion object {
        const val PUMP_LIMIT = 600
        const val PUMP_SLEEP_MS = 10L
        /** Frames that cover the reload pause on the test clock. */
        const val PAUSE_FRAMES = 4
    }
}
