package dev.peppy.mobile.sms

import android.app.Application
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.ServerReachability
import dev.peppy.mobile.gateway.SyncPhase
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SyncSummaryComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test
    fun tooLargeShowsTitleBodyAndSixHourRecheckForBeginKind() {
        show(health(historyTooLarge = true), playUpdateAvailable = false)

        compose.onNodeWithText("History too large for this version", useUnmergedTree = true).fetchSemanticsNode()
        compose.onNodeWithText("Update Peppy to finish setting up this phone. Nothing is lost.", useUnmergedTree = true).fetchSemanticsNode()
        compose.onNodeWithTag("sms-sync-too-large-recheck", useUnmergedTree = true)
            .assertTextEquals("Peppy checks again every 6 hours and after an update.")
    }

    @Test
    fun byteCapTooLargeShowsTheSevenDayRecheckInTheSameSlot() {
        show(health(historyTooLarge = true, byteCap = true), playUpdateAvailable = false)

        compose.onNodeWithTag("sms-sync-too-large-recheck", useUnmergedTree = true)
            .assertTextEquals("Peppy checks again every 7 days and after an update.")
    }

    @Test
    fun liveTooLargeUsesTheCatchUpBody() {
        show(health(historyTooLarge = true, syncPhase = SyncPhase.LIVE), playUpdateAvailable = false)

        compose.onNodeWithText("Update Peppy to catch up on your history. Nothing is lost.", useUnmergedTree = true).fetchSemanticsNode()
        assertTrue(compose.onAllNodesWithText("Update Peppy to finish setting up this phone. Nothing is lost.", useUnmergedTree = true)
            .fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun playBuildOffersOpenGooglePlayButtonThatOpensTheMarketListing() {
        show(health(historyTooLarge = true), playUpdateAvailable = true)

        compose.onNodeWithTag("sync-too-large-update").assertTextEquals("Open Google Play")
        compose.onNodeWithTag("sync-too-large-update").performClick()
        val started = shadowOf(ApplicationProvider.getApplicationContext<Application>()).nextStartedActivity
        assertNotNull(started)
        assertEquals("market://details?id=dev.peppy.mobile", started.data.toString())
        assertTrue(compose.onAllNodesWithTag("sync-too-large-guide").fetchSemanticsNodes().isEmpty())
    }

    @Test
    fun nonPlayDistributionShowsTheGuideButtonAndNoStoreButton() {
        show(health(historyTooLarge = true), playUpdateAvailable = false)

        assertTrue(compose.onAllNodesWithTag("sync-too-large-update").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("sync-too-large-guide").assertTextEquals("Android guide")
    }

    @Test
    fun importingShowsSettingUpWithoutAButtonOrWarning() {
        show(health(syncPhase = SyncPhase.BOOTSTRAP), playUpdateAvailable = true)

        compose.onNodeWithText("Setting up", useUnmergedTree = true).fetchSemanticsNode()
        assertTrue(compose.onAllNodesWithTag("sync-too-large-update").fetchSemanticsNodes().isEmpty())
        assertTrue(compose.onAllNodesWithTag("sync-too-large-guide").fetchSemanticsNodes().isEmpty())
    }

    private fun show(health: SyncHealth, playUpdateAvailable: Boolean) {
        compose.setContent {
            MaterialTheme { SyncSummary(health, playUpdateAvailable = playUpdateAvailable) }
        }
    }

    private fun health(
        syncPhase: SyncPhase = SyncPhase.BOOTSTRAP,
        historyTooLarge: Boolean = false,
        byteCap: Boolean = false,
    ): SyncHealth = syncHealth(
        GatewayStatus(
            true, null, true, true, syncPhase,
            historyTooLarge = historyTooLarge, historyTooLargeByteCap = byteCap,
        ),
        receiveGranted = true,
        sendGranted = true,
        readGranted = true,
        accountReadOnly = false,
        smsSyncOn = true,
        history = SmsHistoryState.CAPTURED,
        reachability = ServerReachability.AVAILABLE,
    )
}
