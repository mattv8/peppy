package dev.peppy.mobile.sms

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.RecoveryReason
import dev.peppy.mobile.gateway.SyncPhase
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class StoppedSyncDetailsDialogTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test
    fun lockedMissingDatabaseRoutesToInformationalDetailsInsteadOfSms() {
        assertEquals(SyncDetailsRoute.STOPPED_DETAILS, syncDetailsRoute(databaseMissingStatus(sharedKeysReady = false)))
    }

    @Test
    fun readyStoppedSyncRoutesToSmsDetails() {
        assertEquals(SyncDetailsRoute.SMS, syncDetailsRoute(databaseMissingStatus(sharedKeysReady = true)))
    }

    @Test
    fun dialogExplainsMissingDatabaseAndDismisses() {
        var visible by mutableStateOf(true)
        compose.setContent {
            MaterialTheme {
                if (visible) StoppedSyncDetailsDialog(databaseMissingStatus(sharedKeysReady = false)) { visible = false }
            }
        }

        compose.onNodeWithTag("sms-stopped-details-dialog").fetchSemanticsNode()
        compose.onNodeWithText("This phone's message storage is missing, so nothing will be sent.", substring = true).fetchSemanticsNode()
        compose.onNodeWithTag("sms-stopped-details-dismiss").performClick()
        assertTrue(compose.onAllNodesWithTag("sms-stopped-details-dialog").fetchSemanticsNodes().isEmpty())
    }

    private fun databaseMissingStatus(sharedKeysReady: Boolean) = GatewayStatus(
        enrolled = true,
        origin = null,
        databaseOpen = sharedKeysReady,
        sharedKeysReady = sharedKeysReady,
        syncPhase = SyncPhase.RECOVERY_REQUIRED,
        recoveryReason = RecoveryReason.DATABASE_MISSING,
    )
}
