package dev.peppy.mobile.gateway

import android.content.Context
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performImeAction
import androidx.compose.ui.test.performTextReplacement
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class DevicesPageTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    private val ownerId = "11111111-1111-4111-8111-111111111111"
    private val phoneId = "22222222-2222-4222-8222-222222222222"
    private val gatewayId = "33333333-3333-4333-8333-333333333333"
    private val removedId = "44444444-4444-4444-8444-444444444444"
    private val removedTwoId = "55555555-5555-4555-8555-555555555555"

    private val roster = mutableStateOf(GatewayAccountState(owner(), listOf(owner())))
    private var activity: ComponentActivity? = null

    private fun device(
        id: String,
        role: String = "device",
        revoked: Boolean = false,
        name: String? = "Pixel 8",
        kind: String? = "android",
        presence: DevicePresence? = DevicePresence(DevicePresenceState.CONNECTED, null),
    ) = GatewayAccountDevice(id, role, revoked, 1L, name, kind, presence)

    private fun owner(id: String = ownerId) = device(id, role = "owner", name = "Laptop", kind = "desktop")

    private fun page(
        state: GatewayAccountState,
        accountReadOnly: Boolean = false,
        revoke: suspend (GatewayAccountDevice) -> Boolean = { true },
        rename: suspend (GatewayAccountDevice, String) -> RenameResult = { _, _ -> RenameResult.Renamed },
        onRefresh: () -> Unit = {},
        onChanged: () -> Unit = {},
    ) {
        roster.value = state
        compose.setContent {
            MaterialTheme {
                activity = LocalActivity.current as ComponentActivity
                DevicesPage(
                    state = roster.value,
                    onBack = {},
                    onRefresh = onRefresh,
                    refreshing = false,
                    revoke = revoke,
                    rename = rename,
                    onChanged = onChanged,
                    accountReadOnly = accountReadOnly,
                )
            }
        }
    }

    private fun setRoster(state: GatewayAccountState) = compose.runOnIdle { roster.value = state }

    private fun exists(tag: String) = compose.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty()

    private fun click(tag: String) = compose.onNodeWithTag(tag, useUnmergedTree = true).performClick()

    private fun textOf(tag: String): String =
        compose.onNodeWithTag(tag, useUnmergedTree = true).fetchSemanticsNode()
            .config[SemanticsProperties.Text].joinToString("") { it.text }

    private fun backPress() = compose.runOnIdle { activity!!.onBackPressedDispatcher.onBackPressed() }

    @Test fun ownerMenuForOtherDeviceHasRenameAndRemoveButNoDisconnect() {
        val phone = device(phoneId)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        assertFalse(exists("devices-owner-required"))
        click("device-menu-$phoneId")
        assertTrue(exists("device-rename-$phoneId"))
        assertTrue(exists("device-remove-$phoneId"))
        assertFalse(exists("device-disconnect-$phoneId"))
    }

    @Test fun ownerMenuForSelfHasRenameAndDisconnectButNoRemove() {
        page(GatewayAccountState(self = owner(), devices = listOf(owner())))
        click("device-menu-$ownerId")
        assertTrue(exists("device-rename-$ownerId"))
        assertTrue(exists("device-disconnect-$ownerId"))
        assertFalse(exists("device-remove-$ownerId"))
    }

    @Test fun gatewayCannotRemoveAndSeesOwnerHint() {
        val self = device(gatewayId, role = "gateway", name = "Gateway phone")
        val other = device(phoneId)
        page(GatewayAccountState(self = self, devices = listOf(self, other)))
        assertTrue(exists("devices-owner-required"))
        assertTrue(exists("info-devices-owner-required"))
        click("device-menu-$phoneId")
        assertTrue(exists("device-rename-$phoneId"))
        assertFalse(exists("device-remove-$phoneId"))
    }

    @Test fun gatewayOwnRowOffersDisconnectWithoutRemove() {
        val self = device(gatewayId, role = "gateway", name = "Gateway phone")
        page(GatewayAccountState(self = self, devices = listOf(self)))
        click("device-menu-$gatewayId")
        assertTrue(exists("device-disconnect-$gatewayId"))
        assertFalse(exists("device-remove-$gatewayId"))
    }

    @Test fun badgesShowThisDeviceRoleAndConnectedPresence() {
        page(GatewayAccountState(self = owner(), devices = listOf(owner())))
        val badges = compose.onNodeWithTag("device-badges-$ownerId", useUnmergedTree = true).fetchSemanticsNode()
        assertTrue(hasContentDescription("This device, Connected, Owner").matches(badges))
    }

    @Test fun seenDeviceShowsHoursBadge() {
        val phone = device(phoneId, presence = DevicePresence(DevicePresenceState.SEEN, 7200))
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        compose.onNodeWithText("Last seen 2 hr ago", useUnmergedTree = true).fetchSemanticsNode()
    }

    @Test fun nullPresenceShowsUnknownStatus() {
        val phone = device(phoneId, presence = null)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        compose.onNodeWithText("Status unknown", useUnmergedTree = true).fetchSemanticsNode()
    }

    @Test fun pausedBadgeAppearsOnlyForGatewayRowsWhenReadOnly() {
        val gateway = device(gatewayId, role = "gateway", name = "Gateway phone")
        val plain = device(phoneId)
        val state = GatewayAccountState(self = owner(), devices = listOf(owner(), gateway, plain))
        page(state, accountReadOnly = true)
        assertTrue(exists("device-badge-paused-$gatewayId"))
        assertFalse(exists("device-badge-paused-$phoneId"))
        assertFalse(exists("device-badge-paused-$ownerId"))
    }

    @Test fun pausedBadgeIsHiddenWhenAccountIsNotReadOnly() {
        val gateway = device(gatewayId, role = "gateway", name = "Gateway phone")
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), gateway)))
        assertFalse(exists("device-badge-paused-$gatewayId"))
    }

    @Test fun presenceBadgeMapsEveryPresenceState() {
        assertEquals(PresenceBadge.UNKNOWN to null, presenceBadge(null))
        assertEquals(PresenceBadge.UNKNOWN to null, presenceBadge(DevicePresence(DevicePresenceState.UNKNOWN, 5)))
        assertEquals(PresenceBadge.CONNECTED to null, presenceBadge(DevicePresence(DevicePresenceState.CONNECTED, null)))
        assertEquals(PresenceBadge.NEVER to null, presenceBadge(DevicePresence(DevicePresenceState.NEVER, null)))
        assertEquals(PresenceBadge.UNKNOWN to null, presenceBadge(DevicePresence(DevicePresenceState.SEEN, null)))
        assertEquals(PresenceBadge.SEEN_NOW to null, presenceBadge(DevicePresence(DevicePresenceState.SEEN, 30)))
        assertEquals(PresenceBadge.SEEN_MINUTES to 10L, presenceBadge(DevicePresence(DevicePresenceState.SEEN, 600)))
        assertEquals(PresenceBadge.SEEN_HOURS to 2L, presenceBadge(DevicePresence(DevicePresenceState.SEEN, 7200)))
    }

    @Test fun removedDevicesStayHiddenUntilToggled() {
        val removed = listOf(
            device(removedId, revoked = true, name = "Old phone"),
            device(removedTwoId, revoked = true, name = "Older phone"),
        )
        page(GatewayAccountState(self = owner(), devices = listOf(owner()) + removed))
        compose.onNodeWithText("Previously removed (2)", useUnmergedTree = true).fetchSemanticsNode()
        assertFalse(exists("device-row-$removedId"))
        click("devices-removed-toggle")
        assertTrue(exists("device-row-$removedId"))
        assertTrue(exists("device-row-$removedTwoId"))
        assertFalse(exists("device-menu-$removedId"))
    }

    @Test fun removeConfirmCallsRevokeAndKeepsDialogOnFailure() {
        val phone = device(phoneId)
        val revoked = mutableListOf<String>()
        val succeed = java.util.concurrent.atomic.AtomicBoolean(false)
        val refreshes = AtomicInteger()
        val changes = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            revoke = {
                revoked += it.id
                succeed.get()
            },
            onRefresh = { refreshes.incrementAndGet() },
            onChanged = { changes.incrementAndGet() },
        )
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        click("device-revoke-confirm-$phoneId")
        compose.waitUntil { exists("device-revoke-error") }
        assertEquals(listOf(phoneId), revoked)
        assertTrue(exists("device-revoke-dialog"))
        assertEquals(0, refreshes.get())

        succeed.set(true)
        click("device-revoke-confirm-$phoneId")
        compose.waitUntil { !exists("device-revoke-dialog") }
        assertEquals(1, refreshes.get())
        assertEquals(0, changes.get())
    }

    @Test fun removeDialogForGatewayIncludesGatewayNote() {
        val gateway = device(gatewayId, role = "gateway", name = "Gateway phone")
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), gateway)))
        click("device-menu-$gatewayId")
        click("device-remove-$gatewayId")
        val note = ApplicationProvider.getApplicationContext<Context>().getString(R.string.peppy_devices_remove_gateway_note)
        compose.onNodeWithText(note).fetchSemanticsNode()
    }

    @Test fun disconnectSuccessCallsOnChangedOnly() {
        val self = device(gatewayId, role = "gateway", name = "Gateway phone")
        val refreshes = AtomicInteger()
        val changes = AtomicInteger()
        page(
            GatewayAccountState(self = self, devices = listOf(self)),
            onRefresh = { refreshes.incrementAndGet() },
            onChanged = { changes.incrementAndGet() },
        )
        click("device-menu-$gatewayId")
        click("device-disconnect-$gatewayId")
        click("device-revoke-confirm-$gatewayId")
        compose.waitUntil { changes.get() == 1 }
        assertEquals(0, refreshes.get())
    }

    @Test fun inFlightRemoveDisablesButtonsAndIgnoresDismiss() {
        val phone = device(phoneId)
        val gate = CompletableDeferred<Boolean>()
        val calls = AtomicInteger()
        val refreshes = AtomicInteger()
        val changes = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            revoke = {
                calls.incrementAndGet()
                gate.await()
            },
            onRefresh = { refreshes.incrementAndGet() },
            onChanged = { changes.incrementAndGet() },
        )
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        click("device-revoke-confirm-$phoneId")
        compose.waitUntil { exists("device-revoke-progress") }
        compose.onNodeWithTag("device-revoke-confirm-$phoneId", useUnmergedTree = true).assertIsNotEnabled()
        compose.onNodeWithTag("device-revoke-cancel", useUnmergedTree = true).assertIsNotEnabled()
        backPress()
        assertTrue(exists("device-revoke-dialog"))
        assertEquals(1, calls.get())

        compose.runOnIdle { gate.complete(true) }
        compose.waitUntil { !exists("device-revoke-dialog") }
        assertEquals(1, calls.get())
        assertEquals(1, refreshes.get())
        assertEquals(0, changes.get())
    }

    @Test fun inFlightDisconnectCallsOnChangedOnly() {
        val self = device(gatewayId, role = "gateway", name = "Gateway phone")
        val gate = CompletableDeferred<Boolean>()
        val calls = AtomicInteger()
        val refreshes = AtomicInteger()
        val changes = AtomicInteger()
        page(
            GatewayAccountState(self = self, devices = listOf(self)),
            revoke = {
                calls.incrementAndGet()
                gate.await()
            },
            onRefresh = { refreshes.incrementAndGet() },
            onChanged = { changes.incrementAndGet() },
        )
        click("device-menu-$gatewayId")
        click("device-disconnect-$gatewayId")
        click("device-revoke-confirm-$gatewayId")
        compose.waitUntil { exists("device-revoke-progress") }
        compose.runOnIdle { gate.complete(true) }
        compose.waitUntil { changes.get() == 1 }
        assertEquals(1, calls.get())
        assertEquals(0, refreshes.get())
    }

    @Test fun ineligibleDialogWaitsForInFlightCallThenCloses() {
        val phone = device(phoneId)
        val gate = CompletableDeferred<Boolean>()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            revoke = { gate.await() },
        )
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        click("device-revoke-confirm-$phoneId")
        compose.waitUntil { exists("device-revoke-progress") }
        setRoster(GatewayAccountState(self = owner(), devices = listOf(owner(), phone.copy(revoked = true))))
        assertTrue(exists("device-revoke-dialog"))
        compose.runOnIdle { gate.complete(false) }
        compose.waitUntil { !exists("device-revoke-dialog") }
    }

    @Test fun rosterRevokingTargetClosesOpenRemoveDialog() {
        val phone = device(phoneId)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        assertTrue(exists("device-revoke-dialog"))
        setRoster(GatewayAccountState(self = owner(), devices = listOf(owner(), phone.copy(revoked = true))))
        compose.waitUntil { !exists("device-revoke-dialog") }
    }

    @Test fun selfRoleChangeClosesOpenRemoveDialog() {
        val phone = device(phoneId)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        assertTrue(exists("device-revoke-dialog"))
        val demoted = owner().copy(role = "gateway")
        setRoster(GatewayAccountState(self = demoted, devices = listOf(demoted, phone)))
        compose.waitUntil { !exists("device-revoke-dialog") }
    }

    @Test fun renameSaveIsDisabledForBlankOrUnchangedNames() {
        val phone = device(phoneId)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-save", useUnmergedTree = true).assertIsNotEnabled()
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("")
        compose.onNodeWithTag("device-rename-save", useUnmergedTree = true).assertIsNotEnabled()
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("  Pixel 8  ")
        compose.onNodeWithTag("device-rename-save", useUnmergedTree = true).assertIsNotEnabled()
    }

    @Test fun invalidRenameShowsFieldError() {
        val phone = device(phoneId)
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            rename = { _, _ -> RenameResult.InvalidName },
        )
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("Desk")
        click("device-rename-save")
        compose.waitUntil { exists("device-rename-invalid") }
    }

    @Test fun successfulRenameRefreshesOnlyAndClosesDialog() {
        val phone = device(phoneId)
        val renamed = mutableListOf<String>()
        val refreshes = AtomicInteger()
        val changes = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            rename = { _, name ->
                renamed += name
                RenameResult.Renamed
            },
            onRefresh = { refreshes.incrementAndGet() },
            onChanged = { changes.incrementAndGet() },
        )
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("Desk")
        click("device-rename-save")
        compose.waitUntil { !exists("device-rename-dialog") }
        assertEquals(listOf("Desk"), renamed)
        assertEquals(1, refreshes.get())
        assertEquals(0, changes.get())
    }

    @Test fun failedRenameThenRetrySucceeds() {
        val phone = device(phoneId)
        val attempts = AtomicInteger()
        val refreshes = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            rename = { _, _ ->
                if (attempts.incrementAndGet() == 1) RenameResult.Failed else RenameResult.Renamed
            },
            onRefresh = { refreshes.incrementAndGet() },
        )
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("Desk")
        click("device-rename-save")
        compose.waitUntil { exists("device-rename-error") }
        assertTrue(exists("device-rename-dialog"))
        assertEquals(0, refreshes.get())

        click("device-rename-save")
        compose.waitUntil { !exists("device-rename-dialog") }
        assertEquals(2, attempts.get())
        assertEquals(1, refreshes.get())
    }

    @Test fun inFlightRenameShowsSpinnerAndIgnoresImeDoneAndSave() {
        val phone = device(phoneId)
        val gate = CompletableDeferred<RenameResult>()
        val calls = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            rename = { _, _ ->
                calls.incrementAndGet()
                gate.await()
            },
        )
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("Desk")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performImeAction()
        compose.waitUntil { exists("device-rename-progress") }
        compose.onNodeWithTag("device-rename-save", useUnmergedTree = true).assertIsNotEnabled()
        compose.onNodeWithTag("device-rename-cancel", useUnmergedTree = true).assertIsNotEnabled()
        backPress()
        assertTrue(exists("device-rename-dialog"))
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performImeAction()
        assertEquals(1, calls.get())

        compose.runOnIdle { gate.complete(RenameResult.Renamed) }
        compose.waitUntil { !exists("device-rename-dialog") }
        assertEquals(1, calls.get())
    }

    /** Invokes the node's captured OnClick twice in one UI-thread turn, before any recomposition. */
    private fun clickTwiceInSameFrame(tag: String) {
        val onClick = requireNotNull(
            compose.onNodeWithTag(tag, useUnmergedTree = true).fetchSemanticsNode()
                .config[SemanticsActions.OnClick].action,
        )
        compose.runOnUiThread {
            onClick()
            onClick()
        }
    }

    @Test fun sameFrameDoubleConfirmCallsRevokeOnce() {
        val phone = device(phoneId)
        val gate = CompletableDeferred<Boolean>()
        val calls = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            revoke = {
                calls.incrementAndGet()
                gate.await()
            },
        )
        click("device-menu-$phoneId")
        click("device-remove-$phoneId")
        clickTwiceInSameFrame("device-revoke-confirm-$phoneId")
        compose.waitUntil { exists("device-revoke-progress") }
        compose.onNodeWithTag("device-revoke-confirm-$phoneId", useUnmergedTree = true).assertIsNotEnabled()
        compose.runOnIdle { gate.complete(true) }
        compose.waitUntil { !exists("device-revoke-dialog") }
        assertEquals(1, calls.get())
    }

    @Test fun sameFrameDoubleRenameSaveCallsRenameOnce() {
        val phone = device(phoneId)
        val gate = CompletableDeferred<RenameResult>()
        val calls = AtomicInteger()
        page(
            GatewayAccountState(self = owner(), devices = listOf(owner(), phone)),
            rename = { _, _ ->
                calls.incrementAndGet()
                gate.await()
            },
        )
        click("device-menu-$phoneId")
        click("device-rename-$phoneId")
        compose.onNodeWithTag("device-rename-field", useUnmergedTree = true).performTextReplacement("Desk")
        clickTwiceInSameFrame("device-rename-save")
        compose.waitUntil { exists("device-rename-progress") }
        compose.runOnIdle { gate.complete(RenameResult.Renamed) }
        compose.waitUntil { !exists("device-rename-dialog") }
        assertEquals(1, calls.get())
    }

    @Test fun unnamedDeviceShowsFallbackName() {
        val phone = device(phoneId, name = null)
        page(GatewayAccountState(self = owner(), devices = listOf(owner(), phone)))
        assertEquals("Unnamed device", textOf("device-name-$phoneId"))
    }
}
