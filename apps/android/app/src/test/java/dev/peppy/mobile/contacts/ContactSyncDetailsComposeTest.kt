package dev.peppy.mobile.contacts

import androidx.compose.material3.MaterialTheme
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
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
class ContactSyncDetailsComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun accountPickerIsHiddenWithOneWritableAccount() {
        render(accounts = listOf(ContactAccountChoice("one", "Personal", true)))
        assertTrue(compose.onAllNodesWithTag("contact-account-list").fetchSemanticsNodes().isEmpty())
    }

    @Test fun accountPickerIsShownWithTwoWritableAccounts() {
        render(accounts = listOf(ContactAccountChoice("one", "Personal", true), ContactAccountChoice("two", "Work", true)))
        compose.onNodeWithTag("contact-account-list").fetchSemanticsNode()
    }

    @Test fun selectedModeAndPolicyDescriptionReflectCurrentPolicy() {
        render(remoteEdits = "confirm")

        listOf("off", "confirm", "auto").forEach { compose.onNodeWithTag("remote-edits-$it").fetchSemanticsNode() }
        compose.onNodeWithTag("remote-edits-confirm").assert(SemanticsMatcher.expectValue(SemanticsProperties.Selected, true))
        compose.onNodeWithTag("remote-edits-policy-description").assertTextEquals("Every change waits for your approval on this phone.")
    }

    @Test @Config(qualifiers = "w480dp-h800dp")
    fun largeFontUsesRadioFallbackAndCallsSelectedMode() {
        var selected = ""
        render(fontScale = 1.4f, onSelectRemoteEdits = { selected = it })

        compose.onNodeWithTag("remote-edits-auto").assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton))
        compose.onNodeWithTag("remote-edits-auto").performClick()
        compose.runOnIdle { assertEquals("auto", selected) }
    }

    @Test fun narrowWidthUsesRadioFallback() {
        render(widthDp = 360)

        compose.onNodeWithTag("remote-edits-auto").assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton))
    }

    @Test fun pendingApprovalRendersActionsAndApprovesItem() {
        val item = ContactPendingItem("request-1", "edit", "requested", "Ada", emptyList())
        var decision: Pair<ContactPendingItem, Boolean>? = null
        render(pending = listOf(item), onDecide = { request, approve -> decision = request to approve })

        compose.onNodeWithTag("contact-request-request-1-approve").performClick()
        compose.onNodeWithTag("contact-request-request-1-reject").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(item to true, decision) }
    }

    @Test fun unknownPendingItemCallsResolveCallback() {
        val item = ContactPendingItem("unknown-1", "edit", "outcome_unknown", "Ada", emptyList())
        var resolved: ContactPendingItem? = null
        render(pending = listOf(item), onResolveUnknown = { resolved = it })

        compose.onNodeWithTag("contact-request-unknown-1-resolve").performClick()
        compose.runOnIdle { assertEquals(item, resolved) }
    }

    @Test fun displaysActionError() {
        render(actionError = "Couldn't save this change. Try again.")

        compose.onNodeWithTag("contact-action-error").assertTextEquals("Couldn't save this change. Try again.")
    }

    @Test fun pageHeaderProvidesBackControlAndHeading() {
        render()

        compose.onNodeWithTag("contacts-details-back").fetchSemanticsNode()
        compose.onNodeWithTag("contacts-details-title").assert(SemanticsMatcher.expectValue(SemanticsProperties.Heading, Unit))
        assertTrue(compose.onAllNodesWithTag("contacts-hero").fetchSemanticsNodes().isEmpty())
    }

    private fun render(
        accounts: List<ContactAccountChoice> = emptyList(),
        remoteEdits: String = "confirm",
        pending: List<ContactPendingItem> = emptyList(),
        fontScale: Float = 1f,
        widthDp: Int = 411,
        onSelectRemoteEdits: (String) -> Unit = {},
        onDecide: (ContactPendingItem, Boolean) -> Unit = { _, _ -> },
        onResolveUnknown: (ContactPendingItem) -> Unit = {},
        actionError: String? = null,
    ) {
        val overview = ContactOverview("book", null, remoteEdits, 3, accounts.firstOrNull()?.id, accounts, pending, null)
        val current = ContactSettingsState(true, false, true, false, true, true, true, false, false, false, false, overview)
        compose.setContent {
            MaterialTheme {
                CompositionLocalProvider(LocalDensity provides Density(1f, fontScale)) {
                    androidx.compose.foundation.layout.Box(Modifier.width(widthDp.dp)) {
                        ContactSyncDetailsContent(
                            current = current,
                            busy = false,
                            readSettingsPending = false,
                            writeSettingsPending = false,
                            onBack = {},
                            onOpenReadSettings = {},
                            onOpenWriteSettings = {},
                            onSelectRemoteEdits = onSelectRemoteEdits,
                            onSelectAccount = {},
                            onDecide = onDecide,
                            onResolveUnknown = onResolveUnknown,
                            actionError = actionError,
                        )
                    }
                }
            }
        }
    }
}
