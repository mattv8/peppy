package dev.peppy.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class AttachmentStorageSettingsComposeTest {
    @get:Rule val compose = createComposeRule()

    @Test fun acknowledgementIsOnlyRequestedForRenderedSupportedNotPresentedDetail() {
        var acknowledgements = 0
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(available(AttachmentStorageNoticeState.NOT_PRESENTED), "scope", false, false, {}, { acknowledgements++ }, {}, {})
            }
        }
        compose.onNodeWithTag("attachment-storage-disclosure").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(1, acknowledgements) }
    }

    @Test fun noEligibleOrUnknownPolicyDetailNeverAcknowledges() {
        var acknowledgements = 0
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(available(AttachmentStorageNoticeState.ELIGIBLE, supported = false), "scope", false, false, {}, { acknowledgements++ }, {}, {})
            }
        }
        compose.onNodeWithTag("attachment-storage-disclosure").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(0, acknowledgements) }
    }

    @Test fun unavailableDetailKeepsRefreshButDoesNotAcknowledge() {
        var acknowledgements = 0
        compose.setContent {
            MaterialTheme {
                AttachmentStorageSettings(AttachmentStorageUiState.Unavailable(null), "scope", false, false, {}, { acknowledgements++ }, {}, {})
            }
        }
        compose.onNodeWithTag("attachment-storage-refresh").fetchSemanticsNode()
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-disclosure").fetchSemanticsNodes().size)
        compose.runOnIdle { assertEquals(0, acknowledgements) }
    }

    private fun available(state: AttachmentStorageNoticeState, supported: Boolean = true) = AttachmentStorageUiState.Available(
        AttachmentStorageSnapshot(1, 2, 1, AttachmentStorageCleanup.DISABLED, null, AttachmentStorageNotice(if (supported) "v1" else "v2", state, null), supported),
    )
}
