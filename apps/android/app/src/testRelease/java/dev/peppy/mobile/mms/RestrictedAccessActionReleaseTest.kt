package dev.peppy.mobile.mms

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.googlemessages.GoogleMessagesLinkTestHooks
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.After
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
class RestrictedAccessActionReleaseTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @After
    fun clearLinkHooks() {
        GoogleMessagesLinkTestHooks.available = null
        GoogleMessagesLinkTestHooks.linked = null
    }

    @Test
    fun `available unlinked access shows link action without adb command`() {
        GoogleMessagesLinkTestHooks.available = true
        GoogleMessagesLinkTestHooks.linked = false
        var opens = 0

        setAction { opens++ }

        compose.onNodeWithTag("mms-restricted-link-body").assertIsDisplayed()
        compose.onNodeWithTag("mms-restricted-link-action").performClick()
        compose.onNodeWithTag("mms-restricted-message-command").assertDoesNotExist()
        assertEquals(1, opens)
    }

    @Test
    fun `linked access shows manage action`() {
        GoogleMessagesLinkTestHooks.available = true
        GoogleMessagesLinkTestHooks.linked = true

        setAction()

        compose.onNodeWithTag("mms-restricted-link-linked").assertIsDisplayed()
        compose.onNodeWithTag("mms-restricted-link-manage").assertIsDisplayed()
        compose.onNodeWithTag("mms-restricted-link-action").assertDoesNotExist()
        compose.onNodeWithText("Via Google Messages link").assertIsDisplayed()
    }

    @Test
    fun `unavailable access shows plain sentence`() {
        GoogleMessagesLinkTestHooks.available = false

        setAction()

        compose.onNodeWithTag("mms-restricted-link-unavailable").assertIsDisplayed()
        compose.onNodeWithTag("mms-restricted-link-action").assertDoesNotExist()
        compose.onNodeWithTag("mms-restricted-link-manage").assertDoesNotExist()
    }

    @Test
    fun `unknown link state does not briefly show a link action`() {
        compose.setContent {
            MaterialTheme { RestrictedAccessAction(linked = null, available = true, onOpenGoogleMessagesLink = {}) }
        }

        compose.onNodeWithTag("mms-restricted-link-action").assertDoesNotExist()
        compose.onNodeWithTag("mms-restricted-link-manage").assertDoesNotExist()
    }

    @Test
    fun `status and allowed rescan remain available`() {
        compose.setContent {
            MaterialTheme {
                RestrictedMessageAccessControls(
                    context = ApplicationProvider.getApplicationContext(),
                    refresh = 0,
                    access = RestrictedMessageAccess.ALLOWED,
                    showAccessStatus = true,
                    showRescan = true,
                    enabled = true,
                    pending = false,
                    feedback = null,
                    onOpenGoogleMessagesLink = {},
                    startRescan = {},
                )
            }
        }

        compose.onNodeWithTag("mms-restricted-message-access").assertIsDisplayed()
        compose.onNodeWithTag("mms-restricted-message-rescan").assertIsDisplayed()
    }

    @Test
    fun `status help omits adb instructions`() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        compose.setContent {
            MaterialTheme { RestrictedMessageAccessStatus(RestrictedMessageAccess.NOT_ALLOWED) }
        }

        compose.onNodeWithTag("info-mms-restricted-message-access").performClick()

        compose.onNodeWithText(context.getString(R.string.peppy_mms_restricted_play_help)).assertIsDisplayed()
        assertTrue(compose.onAllNodesWithText("adb", substring = true).fetchSemanticsNodes().isEmpty())
    }

    private fun setAction(onOpen: () -> Unit = {}) {
        compose.setContent {
            MaterialTheme {
                Column {
                    RestrictedNotAllowedAccessControls(
                        context = ApplicationProvider.getApplicationContext(),
                        refresh = 0,
                        onOpenGoogleMessagesLink = onOpen,
                    )
                }
            }
        }
    }
}
