package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NotificationAppFiltersScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private class Actions(initial: NotificationAppFilterLoadResult) : NotificationAppFilterActions {
        var result = initial
        var saves = 0
        var refreshes = 0
        var saveResult = true
        override suspend fun load(): NotificationAppFilterLoadResult {
            refreshes += 1
            return result
        }
        override suspend fun setMuted(app: ObservedNotificationApp, muted: Boolean): Boolean {
            saves += 1
            return saveResult
        }
    }

    private class ControlledActions(private val app: ObservedNotificationApp) : NotificationAppFilterActions {
        val started = CompletableDeferred<Unit>()
        val completion = CompletableDeferred<Boolean>()
        var saves = 0
        var savedApp: ObservedNotificationApp? = null
        var savedMuted: Boolean? = null

        override suspend fun load() = NotificationAppFilterLoadResult.Ready(listOf(app), emptySet())

        override suspend fun setMuted(app: ObservedNotificationApp, muted: Boolean): Boolean {
            saves += 1
            savedApp = app
            savedMuted = muted
            started.complete(Unit)
            return completion.await()
        }
    }

    private val chat = ObservedNotificationApp("dev.chat", "Chat")
    private val mail = ObservedNotificationApp("dev.mail", "Mail")
    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test fun searchMatchesLabelAndPackageAndShowsNoMatchFeedback() {
        val actions = Actions(NotificationAppFilterLoadResult.Ready(listOf(chat, mail), setOf("dev.mail")))
        show(actions)

        compose.onNodeWithTag("app-filter-dev.chat").assertIsDisplayed()
        compose.onNodeWithTag("notification-app-search").performTextInput("DEV.MAIL")
        compose.onNodeWithTag("app-filter-dev.mail").assertIsDisplayed()
        compose.onNodeWithTag("app-filter-dev.chat").assertDoesNotExist()
        compose.onNodeWithTag("notification-app-search").performTextClearance()
        compose.onNodeWithTag("notification-app-search").performTextInput("chat")
        compose.onNodeWithTag("app-filter-dev.chat").assertIsDisplayed()
        compose.onNodeWithTag("notification-app-search").performTextClearance()
        compose.onNodeWithTag("notification-app-search").performTextInput("none")
        compose.onNodeWithTag("app-filter-no-match").assertIsDisplayed()
    }

    @Test fun togglePersistsMutedStateAndDisablesSwitchWhileSaving() {
        val actions = ControlledActions(chat)
        show(actions)

        compose.onNodeWithTag("app-filter-switch-dev.chat").assertIsOn().performClick()
        compose.waitUntil(5_000) { actions.started.isCompleted }
        compose.onNodeWithTag("app-filter-switch-dev.chat").assertIsNotEnabled()
        actions.completion.complete(true)
        compose.waitForIdle()
        compose.onNodeWithTag("app-filter-switch-dev.chat").assertIsOff()
        assertEquals(1, actions.saves)
        assertEquals(chat, actions.savedApp)
        assertEquals(true, actions.savedMuted)
    }

    @Test fun failedSaveLeavesPriorCheckedStateAndShowsError() {
        val actions = ControlledActions(chat)
        show(actions)

        compose.onNodeWithTag("app-filter-switch-dev.chat").performClick()
        compose.waitUntil(5_000) { actions.started.isCompleted }
        actions.completion.complete(false)
        compose.waitForIdle()

        compose.onNodeWithTag("notification-filter-error").assertIsDisplayed()
        compose.onNodeWithTag("app-filter-switch-dev.chat").assertIsOn()
        assertEquals(1, actions.saves)
    }

    @Test fun failedSaveThenRefreshClearsSaveErrorAndKeepsLoadErrorDistinct() {
        val actions = Actions(NotificationAppFilterLoadResult.Ready(listOf(chat), emptySet()))
        actions.saveResult = false
        show(actions)

        compose.onNodeWithTag("app-filter-switch-dev.chat").performClick()
        compose.onNodeWithTag("notification-filter-error").assertIsDisplayed()

        actions.result = NotificationAppFilterLoadResult.Error
        compose.onNodeWithTag("notification-refresh-apps").performClick()
        compose.onNodeWithTag("notification-filter-load-error").assertIsDisplayed()
        compose.onNodeWithTag("notification-filter-error").assertDoesNotExist()

        actions.result = NotificationAppFilterLoadResult.Ready(listOf(chat), emptySet())
        compose.onNodeWithTag("notification-refresh-apps").performClick()
        compose.onNodeWithTag("app-filter-switch-dev.chat").assertIsOn()
        compose.onNodeWithTag("notification-filter-error").assertDoesNotExist()
        compose.onNodeWithTag("notification-filter-load-error").assertDoesNotExist()
        assertEquals(1, actions.saves)
        assertEquals(3, actions.refreshes)
    }

    @Test fun lockedEmptyAndLoadingStatesAreDistinctAndRefreshReloads() {
        val actions = Actions(NotificationAppFilterLoadResult.Loading)
        show(actions)
        compose.onNodeWithTag("app-filter-loading").assertIsDisplayed()

        actions.result = NotificationAppFilterLoadResult.Locked
        compose.onNodeWithTag("notification-refresh-apps").performClick()
        compose.onNodeWithTag("app-filter-locked").assertIsDisplayed()

        actions.result = NotificationAppFilterLoadResult.Ready(emptyList(), emptySet())
        compose.onNodeWithTag("notification-refresh-apps").performClick()
        compose.onNodeWithTag("app-filter-empty").assertIsDisplayed()

        actions.result = NotificationAppFilterLoadResult.Error
        compose.onNodeWithTag("notification-refresh-apps").performClick()
        compose.onNodeWithTag("notification-filter-load-error").assertIsDisplayed()
    }

    @Test fun toolbarAndSystemBackInvokeBackCallback() {
        var backs = 0
        val actions = Actions(NotificationAppFilterLoadResult.Ready(emptyList(), emptySet()))
        show(actions) { backs += 1 }

        compose.onNodeWithTag("app-filters-back").performClick()
        compose.activity.runOnUiThread { compose.activity.onBackPressedDispatcher.onBackPressed() }
        compose.waitForIdle()
        assertEquals(2, backs)
    }

    @Test fun navigationRowGatesEntryUntilReadyAndBindsMutedSummary() {
        var opens = 0
        var summary by mutableStateOf<NotificationAppFilterSummaryState>(NotificationAppFilterSummaryState.Loading)
        compose.setContent {
            NotificationAppFiltersNavRow(summary, onOpenAppFilters = { opens += 1 })
        }
        compose.onNodeWithTag("app-filters-nav-row").assertIsNotEnabled()
        assertNavSummary(R.string.peppy_app_filters_loading)

        compose.runOnUiThread { summary = NotificationAppFilterSummaryState.Locked }
        compose.onNodeWithTag("app-filters-nav-row").assertIsNotEnabled()
        assertNavSummary(R.string.peppy_app_filters_locked)

        compose.runOnUiThread { summary = NotificationAppFilterSummaryState.Error }
        compose.onNodeWithTag("app-filters-nav-row").assertIsNotEnabled()
        assertNavSummary(R.string.peppy_app_filters_load_error)

        compose.runOnUiThread { summary = NotificationAppFilterSummaryState.Ready(emptySet()) }
        compose.onNodeWithTag("app-filters-nav-row").assertIsEnabled()
        assertNavSummary(R.string.peppy_app_filters_all_allowed)

        compose.runOnUiThread { summary = NotificationAppFilterSummaryState.Ready(setOf("one")) }
        assertNavSummary(R.string.peppy_app_filters_muted_one)

        compose.runOnUiThread { summary = NotificationAppFilterSummaryState.Ready(setOf("one", "two")) }
        assertNavSummary(R.string.peppy_app_filters_muted_summary, 2)
        compose.onNodeWithTag("app-filters-nav-row").performClick()
        assertEquals(1, opens)
    }

    private fun assertNavSummary(id: Int, vararg args: Any) {
        compose.onNodeWithTag("app-filters-summary", useUnmergedTree = true).assertTextEquals(context.getString(id, *args))
    }

    private fun show(actions: NotificationAppFilterActions, onBack: () -> Unit = {}) {
        compose.setContent { NotificationAppFiltersScreen(onBack = onBack, actions = actions) }
    }
}
