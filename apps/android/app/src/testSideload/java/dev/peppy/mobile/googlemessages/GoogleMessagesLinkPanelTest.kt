package dev.peppy.mobile.googlemessages

import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanel
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.theme.PeppyTheme
import org.junit.After
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkPanelTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @After fun tearDown() {
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        GoogleMessagesLinkTestHooks.available = null
    }

    @Test fun `unavailable engine renders nothing`() {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = false
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = error("unavailable")
        }
        GoogleMessagesLinkTestHooks.available = false
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        check(compose.onAllNodesWithTag("google-messages-link-panel").fetchSemanticsNodes().isEmpty())
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

    @Test fun `linked link presents health actions`() = panel("linked") {
        compose.onNodeWithTag("google-messages-link-linked").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-check-now").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
        compose.onNodeWithTag("google-messages-link-battery-row").fetchSemanticsNode()
    }

    @Test fun `repair state offers owner actions`() {
        panel("needs_relink") {
            compose.onNodeWithTag("google-messages-link-relink").fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-unlink").fetchSemanticsNode()
            compose.onNodeWithTag("google-messages-link-remove-local").fetchSemanticsNode()
        }
    }

    @Test fun `signed out state never offers unlink`() = panel("signed_out") {
        compose.onNodeWithTag("google-messages-link-sign-in-again").fetchSemanticsNode()
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
    }

    private fun panel(state: String, assertion: () -> Unit) {
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
                override fun status() = """{"seq":1,"state":"$state","running":true,"healthy":true,"phone_responding":true,"last_ok_ms":0,"stop_code":""}"""
            }
        }
        LinkRuntimeTestHooks.storeFactory = { InMemoryStore() }
        compose.setContent { PeppyTheme { GoogleMessagesLinkPanel {} } }
        compose.waitUntil { compose.onAllNodesWithTag("google-messages-link-panel").fetchSemanticsNodes().isNotEmpty() }
        assertion()
    }

    private class InMemoryStore : LinkStateStorage {
        override fun read() = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }
}
