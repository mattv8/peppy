package dev.peppy.mobile.attachments

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import dev.peppy.mobile.gateway.AttachmentStorageSettings
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class AttachmentStorageSettingsComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun infoDialogOpensAndDismissesWithoutRefreshingStorage() {
        var refreshes = 0
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(available(), "scope", false, { refreshes++ }, {})
            }
        }

        compose.onNodeWithTag("attachment-storage-info-button").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-info-dismiss").performClick()
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-dialog").fetchSemanticsNodes().size)
        compose.runOnIdle { assertEquals(0, refreshes) }
    }

    @Test fun activeNearFullStorageOffersLearnMore() {
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(available(usedBytes = 90), "scope", false, {}, {})
            }
        }

        compose.onNodeWithTag("attachment-storage-capacity-notice").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-learn-more").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
    }

    @Test fun blockedNearFullStorageOffersLearnMoreWithoutRefreshingStorage() {
        var refreshes = 0
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(blockedNearFull(), "scope", false, { refreshes++ }, {})
            }
        }

        compose.onNodeWithTag("attachment-storage-capacity-notice").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-cause").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-learn-more").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(0, refreshes) }
    }

    @Test fun unavailableStorageHidesTheInformationAction() {
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(AttachmentStorageUiState.Unavailable(null), "scope", false, {}, {})
            }
        }

        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-button").fetchSemanticsNodes().size)
    }

    private fun available(usedBytes: Long = 1) = AttachmentStorageUiState.Available(
        AttachmentStorageSnapshot(usedBytes, 100, 1, AttachmentStorageCleanup.AVAILABLE, null),
    )

    private fun blockedNearFull() = AttachmentStorageUiState.Available(
        AttachmentStorageSnapshot(90, 100, 1, AttachmentStorageCleanup.BLOCKED, AttachmentStorageUnavailableReason.DEVICE_NOT_READY),
    )
}
