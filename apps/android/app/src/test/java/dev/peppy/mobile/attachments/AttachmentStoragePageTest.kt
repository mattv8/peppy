package dev.peppy.mobile.attachments

import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AttachmentStoragePageTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test fun barLevelsFollowUsageThresholds() {
        assertNull(storageBarLevel(0, 0))
        assertNull(storageBarLevel(-1, 10))
        assertEquals(StorageBarLevel.NORMAL, storageBarLevel(89, 100))
        assertEquals(StorageBarLevel.WARNING, storageBarLevel(90, 100))
        assertEquals(StorageBarLevel.FULL, storageBarLevel(100, 100))
        assertEquals(StorageBarLevel.FULL, storageBarLevel(120, 100))
    }

    @Test fun playAvailableShowsUsageAndIncludedBreakdownWithoutManageRow() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 1_200_000_000, limit = 50_000_000_000, base = 50_000_000_000)))

        compose.onNodeWithText(string(R.string.peppy_attachment_storage_used_of_limit, "1.2 GB", "50.0 GB")).fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-breakdown").assertTextEquals(string(R.string.peppy_attachment_storage_included_only, "50.0 GB"))
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-manage").fetchSemanticsNodes().size)
    }

    @Test fun manageRowKeepsBillingHandoffInInfoDialogNotInlineSummary() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 1_200_000_000, limit = 50_000_000_000, base = 50_000_000_000)), showPurchaseActions = true)

        compose.onNodeWithTag("attachment-storage-manage").fetchSemanticsNode()
        assertEquals(0, compose.onAllNodesWithText(string(R.string.peppy_attachment_storage_billing_handoff)).fetchSemanticsNodes().size)
        compose.onNodeWithTag("info-attachment-storage-manage").performClick()
        compose.onNodeWithText(string(R.string.peppy_attachment_storage_billing_handoff)).fetchSemanticsNode()
    }

    @Test fun addOnLimitUsesAddOnBreakdown() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 10_000_000_000, limit = 100_000_000_000, base = 50_000_000_000)))

        compose.onNodeWithTag("attachment-storage-breakdown").assertTextEquals(
            string(R.string.peppy_attachment_storage_included_plus_addon, "50.0 GB", "50.0 GB"),
        )
    }

    @Test fun nearFullCalloutOpensInfoDialogWithPurchaseParagraphAndDismisses() {
        setPage(
            AttachmentStorageUiState.Available(snapshot(used = 95_000_000_000, limit = 100_000_000_000, base = 50_000_000_000)),
            showPurchaseActions = true,
        )

        compose.onNodeWithTag("attachment-storage-capacity-notice-action").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
        compose.onNodeWithText(string(R.string.peppy_attachment_storage_info_body_purchase)).fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-info-dismiss").performClick()
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-dialog").fetchSemanticsNodes().size)
    }

    @Test fun purchaseParagraphIsAbsentWhenPurchaseActionsAreHidden() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 95_000_000_000, limit = 100_000_000_000, base = 50_000_000_000)))

        compose.onNodeWithTag("attachment-storage-capacity-notice-action").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
        assertEquals(0, compose.onAllNodesWithText(string(R.string.peppy_attachment_storage_info_body_purchase)).fetchSemanticsNodes().size)
    }

    @Test fun unavailableShowsRefreshCalloutAndStaleValues() {
        setPage(AttachmentStorageUiState.Unavailable(snapshot(used = 1_000_000_000, limit = 50_000_000_000, base = 50_000_000_000)))

        compose.onNodeWithText(string(R.string.peppy_attachment_storage_unavailable)).fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-stale-label").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-stale-used-of-limit")
            .assertTextEquals(string(R.string.peppy_attachment_storage_used_of_limit, "1.0 GB", "50.0 GB"))
    }

    @Test fun refreshCallsOnRefreshWhenIdle() {
        var refreshes = 0
        setPage(AttachmentStorageUiState.Available(snapshot(used = 1, limit = 10, base = 10)), onRefresh = { refreshes++ })

        compose.onNodeWithTag("attachment-storage-refresh").performClick()
        compose.runOnIdle { assertEquals(1, refreshes) }
    }

    @Test fun refreshIsDisabledWhileBusy() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 1, limit = 10, base = 10)), busy = true)

        compose.onNodeWithTag("attachment-storage-refresh").assertIsNotEnabled()
    }

    @Test fun loadingShowsTextAndProgressIndicator() {
        setPage(AttachmentStorageUiState.Loading)

        compose.onNodeWithTag("attachment-storage-loading").assertTextEquals(string(R.string.peppy_attachment_storage_loading))
        compose.onNodeWithTag("attachment-storage-progress").fetchSemanticsNode()
    }

    @Test fun unsupportedShowsUnavailableText() {
        setPage(AttachmentStorageUiState.Unsupported)

        compose.onNodeWithTag("attachment-storage-unavailable").assertTextEquals(string(R.string.peppy_attachment_storage_unavailable))
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-button").fetchSemanticsNodes().size)
    }

    @Test fun infoActionIsOnlyOfferedWhenAvailable() {
        setPage(AttachmentStorageUiState.Loading)
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-button").fetchSemanticsNodes().size)
    }

    @Test fun infoActionIsAbsentWhenUnavailable() {
        setPage(AttachmentStorageUiState.Unavailable(null))
        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-info-button").fetchSemanticsNodes().size)
    }

    @Test fun infoActionOpensDialogWhenAvailable() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 1, limit = 10, base = 10)))

        compose.onNodeWithTag("attachment-storage-info-button").performClick()
        compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()
    }

    @Test fun unavailableCalloutActionCallsOnRefresh() {
        var refreshes = 0
        setPage(AttachmentStorageUiState.Unavailable(null), onRefresh = { refreshes++ })

        compose.onNodeWithTag("attachment-storage-unavailable-action").performClick()
        compose.runOnIdle { assertEquals(1, refreshes) }
    }

    @Test fun unavailableCalloutActionDoesNotRefreshWhileBusy() {
        var refreshes = 0
        setPage(AttachmentStorageUiState.Unavailable(null), busy = true, onRefresh = { refreshes++ })

        compose.onNodeWithTag("attachment-storage-unavailable-action").performClick()
        compose.runOnIdle { assertEquals(0, refreshes) }
    }

    @Test fun zeroLimitDrawsNoUsageBar() {
        setPage(AttachmentStorageUiState.Available(snapshot(used = 0, limit = 0, base = 0)))

        assertEquals(0, compose.onAllNodesWithTag("attachment-storage-bar", useUnmergedTree = true).fetchSemanticsNodes().size)
    }

    @Test fun pageTitleUsesShortStorageTitle() {
        setPage(AttachmentStorageUiState.Loading)

        compose.onNodeWithText(string(R.string.peppy_account_storage_title)).fetchSemanticsNode()
    }

    private fun setPage(
        state: AttachmentStorageUiState,
        busy: Boolean = false,
        showPurchaseActions: Boolean = false,
        onRefresh: () -> Unit = {},
    ) {
        compose.setContent {
            MaterialTheme {
                AttachmentStoragePage(
                    state = state,
                    scope = "scope",
                    busy = busy,
                    onBack = {},
                    onRefresh = onRefresh,
                    onManage = {},
                    showPurchaseActions = showPurchaseActions,
                )
            }
        }
    }

    private fun snapshot(used: Long, limit: Long, base: Long) = AttachmentStorageSnapshot(
        usedBytes = used,
        limitBytes = limit,
        baseBytes = base,
        cleanup = AttachmentStorageCleanup.AVAILABLE,
        unavailableReason = null,
    )

    private fun string(id: Int, vararg args: Any): String = context.getString(id, *args)
}
