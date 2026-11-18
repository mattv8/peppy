package dev.peppy.mobile.gateway

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.SemanticsConfiguration
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsPropertyKey
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.text.NumberFormat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SetupProgressCardTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Before
    fun resetTracker() = SetupFinishTracker.resetForTest()

    @Test
    fun checkingShowsIndeterminateBarAndNoCount() {
        show(status(SetupProgress.Checking))

        compose.onNodeWithTag("sync-setup-card").fetchSemanticsNode()
        compose.onNodeWithText("Checking history", useUnmergedTree = true).fetchSemanticsNode()
        assertNull(unmerged("sync-setup-count"))
        assertEquals(ProgressBarRangeInfo.Indeterminate, bar().config.prop(SemanticsProperties.ProgressBarRangeInfo))
        assertEquals("Checking history", bar().config.prop(SemanticsProperties.StateDescription))
        compose.onNodeWithText("Other devices can't send through this phone until this finishes.", useUnmergedTree = true).fetchSemanticsNode()
    }

    @Test
    fun downloadingShowsDeterminateBarAndCountWithGroupingAndState() {
        show(status(SetupProgress.Downloading(staged = 42_000, expected = 108_882)))

        val count = formatOf(42_000) + " of " + formatOf(108_882) + " items"
        compose.onNodeWithText(count, useUnmergedTree = true).fetchSemanticsNode()
        val range = bar().config.prop(SemanticsProperties.ProgressBarRangeInfo)
        assertNotNull(range)
        assertEquals(42_000f / 108_882f, range!!.current, 0.0001f)
        assertEquals(count, bar().config.prop(SemanticsProperties.StateDescription))
    }

    @Test
    fun downloadingCountNeverExceedsItsTotal() {
        show(status(SetupProgress.Downloading(staged = 150, expected = 100)))

        compose.onNodeWithText(formatOf(100) + " of " + formatOf(100) + " items", useUnmergedTree = true).fetchSemanticsNode()
    }

    @Test
    fun organizingShowsIndeterminateBarAndItemsLeft() {
        show(status(SetupProgress.Organizing(remaining = 31_500)))

        compose.onNodeWithText(formatOf(31_500) + " items left", useUnmergedTree = true).fetchSemanticsNode()
        assertEquals(ProgressBarRangeInfo.Indeterminate, bar().config.prop(SemanticsProperties.ProgressBarRangeInfo))
    }

    @Test
    fun unknownTotalKeepsBarIndeterminateAndHidesCount() {
        show(status(SetupProgress.Downloading(staged = 0, expected = 0)))

        assertNull(unmerged("sync-setup-count"))
        assertEquals(ProgressBarRangeInfo.Indeterminate, bar().config.prop(SemanticsProperties.ProgressBarRangeInfo))
        assertEquals("Downloading history", bar().config.prop(SemanticsProperties.StateDescription))
    }

    @Test
    fun importingWithoutPersistedProgressShowsTheCheckingPresentation() {
        show(status(progress = null))

        compose.onNodeWithText("Checking history", useUnmergedTree = true).fetchSemanticsNode()
        assertEquals(ProgressBarRangeInfo.Indeterminate, bar().config.prop(SemanticsProperties.ProgressBarRangeInfo))
    }

    @Test
    fun countsNeverAnnounceAndOnlyFinishedTextIsLive() {
        show(status(SetupProgress.Organizing(remaining = 5)))

        assertNull(unmerged("sync-setup-count")?.config?.prop(SemanticsProperties.LiveRegion))
        assertNull(bar().config.prop(SemanticsProperties.LiveRegion))
    }

    @Test
    fun hiddenWhileLocked() {
        show(status(SetupProgress.Checking, unlocked = false))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun hiddenWhenNoSetupIsRunning() {
        show(status(progress = null, phase = SyncPhase.LIVE))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun hiddenWhenLiveEvenWithStaleProgress() {
        show(status(SetupProgress.Downloading(staged = 10, expected = 20), phase = SyncPhase.LIVE))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun hiddenWhenStoppedEvenWithAnActiveImport() {
        show(status(SetupProgress.Checking, outboxRejection = "http_403"))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun hiddenWhenHistoryIsTooLarge() {
        show(status(SetupProgress.Downloading(staged = 98_000, expected = 108_882), historyTooLarge = true))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun finishedNoticeShowsOnceAfterAnInSessionImportThenRemovesAfterSixSeconds() {
        compose.mainClock.autoAdvance = false
        SetupFinishTracker.observe(status(SetupProgress.Organizing(remaining = 3)))
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE))
        show(status(progress = null, phase = SyncPhase.LIVE))
        compose.mainClock.advanceTimeBy(1)

        val done = compose.onNodeWithTag("sync-setup-done", useUnmergedTree = true).fetchSemanticsNode()
        assertNotNull(done)
        compose.onNodeWithText("Other devices can now send through this phone.", useUnmergedTree = true).fetchSemanticsNode()

        compose.mainClock.advanceTimeBy(SETUP_FINISHED_NOTICE_MS + 1)
        compose.waitForIdle()
        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun finishedNoticeIsOneMergedLiveRegionWithTitleAndBody() {
        compose.mainClock.autoAdvance = false
        SetupFinishTracker.observe(status(SetupProgress.Checking))
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE))
        show(status(progress = null, phase = SyncPhase.LIVE))
        compose.mainClock.advanceTimeBy(1)

        val merged = compose.onNode(hasLiveRegion(), useUnmergedTree = false).fetchSemanticsNode()
        val text = merged.config.prop(SemanticsProperties.Text)?.joinToString(" ") { it.text }.orEmpty()
        assertTrue(text, text.contains("Setup finished"))
        assertTrue(text, text.contains("Other devices can now send through this phone."))
        assertTrue(text, merged.config.prop(SemanticsProperties.LiveRegion) == LiveRegionMode.Polite)
    }

    @Test
    fun coldStartIntoLiveShowsNothing() {
        show(status(progress = null, phase = SyncPhase.LIVE))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun setupEndingInErrorStopShowsNoFinishedNotice() {
        SetupFinishTracker.observe(status(SetupProgress.Checking))
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.RECOVERY_REQUIRED))
        show(status(progress = null, phase = SyncPhase.RECOVERY_REQUIRED))

        assertTrue(compose.onAllNodesWithTag("sync-setup-card").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun formattingUsesLocaleGroupingAndGuardsZeroTotal() {
        assertEquals(0.5f, setupFraction(staged = 50, expected = 100)!!, 0.0001f)
        assertEquals(1f, setupFraction(staged = 150, expected = 100)!!, 0.0001f)
        assertEquals(0f, setupFraction(staged = -5, expected = 100)!!, 0.0001f)
        assertNull(setupFraction(staged = 0, expected = 0))
        assertEquals("108,882", formatSetupCount(108_882, NumberFormat.getIntegerInstance(java.util.Locale.US)))
    }

    @Test
    fun trackerQueuesOneNoticeOnlyForAnObservedImport() {
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE))
        assertFalse(SetupFinishTracker.pending.value)

        SetupFinishTracker.observe(status(SetupProgress.Checking))
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE))
        assertTrue(SetupFinishTracker.pending.value)
        assertTrue(SetupFinishTracker.takeFresh())
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE))
        assertFalse(SetupFinishTracker.pending.value)
    }

    @Test
    fun queuedFinishedNoticeExpiresAfterThirtySeconds() {
        SetupFinishTracker.observe(status(SetupProgress.Checking), nowMs = 0L)
        SetupFinishTracker.observe(status(progress = null, phase = SyncPhase.LIVE), nowMs = 1_000L)

        assertFalse("a notice nobody saw in time is dropped", SetupFinishTracker.takeFresh(nowMs = 1_000L + SETUP_FINISHED_QUEUE_MS + 1))
        assertFalse(SetupFinishTracker.pending.value)
    }

    private fun show(status: GatewayStatus?) {
        compose.setContent {
            MaterialTheme { SetupProgressCard(status) }
        }
    }

    private fun bar(): SemanticsNode = unmerged("sync-setup-progress")!!

    private fun unmerged(tag: String): SemanticsNode? =
        compose.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().firstOrNull()

    /** Reads a semantics property when set; the node's configuration has no nullable accessor in this Compose version. */
    private fun <T> SemanticsConfiguration.prop(key: SemanticsPropertyKey<T>): T? = if (contains(key)) get(key) else null

    private fun hasLiveRegion() = androidx.compose.ui.test.SemanticsMatcher.expectValue(SemanticsProperties.LiveRegion, LiveRegionMode.Polite)

    private fun formatOf(value: Long) = formatSetupCount(value, NumberFormat.getIntegerInstance())

    private fun status(
        progress: SetupProgress? = SetupProgress.Checking,
        unlocked: Boolean = true,
        phase: SyncPhase = SyncPhase.BOOTSTRAP,
        outboxRejection: String? = null,
        historyTooLarge: Boolean = false,
    ) = GatewayStatus(
        enrolled = true,
        origin = null,
        databaseOpen = unlocked,
        sharedKeysReady = unlocked,
        syncPhase = phase,
        outboxRejection = outboxRejection,
        setupProgress = progress,
        historyTooLarge = historyTooLarge,
    )
}
