package dev.peppy.mobile.settings

import android.Manifest
import android.app.Application
import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.R
import dev.peppy.mobile.account.HostedAccountDeletionRow
import dev.peppy.mobile.billing.PlayDeletionContext
import dev.peppy.mobile.gateway.GatewayAccountDevice
import dev.peppy.mobile.gateway.GatewayAccountState
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.gateway.SyncPhase
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SettingsScreenTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test fun ownerHostedPlayWithKeysReadyShowsSyncDangerAndAdvancedOnly() {
        render(status(HOSTED, keysReady = true), roster("owner"))

        compose.waitUntil(5_000) { exists("vault-delete-row") }
        compose.onNodeWithTag("settings-sync-section").fetchSemanticsNode()
        compose.onNodeWithTag("remote-messaging-toggle").fetchSemanticsNode()
        compose.onNodeWithTag("settings-advanced-row").fetchSemanticsNode()
        compose.onNodeWithTag("settings-group-danger").fetchSemanticsNode()
        compose.onNodeWithTag("account-delete-row").fetchSemanticsNode()
        absent("settings-subscription-row")
        absent("add-phone-row")
        absent("connect-computer-row")
    }

    @Test fun gatewaySelfHostedWithKeysReadyShowsLiveSyncAndRelayAdvanced() {
        render(status(SELF_HOSTED, keysReady = true), roster("gateway"))

        compose.waitUntil(5_000) { exists("remote-messaging-toggle") }
        absent("settings-group-danger")
        compose.onNodeWithTag("settings-advanced-row").assertTextContains(relaySummary(), substring = true)

        compose.onNodeWithTag("settings-advanced-row").performScrollTo().performClick()
        compose.waitUntil(5_000) { exists("settings-relay-section") }
    }

    @Test fun lockedHostedPlayShowsPlanAdvancedAndAccountDeletionOnly() {
        render(status(HOSTED, keysReady = false), roster = null)

        compose.waitUntil(5_000) { exists("settings-advanced-row") }
        compose.onNodeWithTag("settings-subscription-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-delete-row").fetchSemanticsNode()
        absent("vault-delete-row")
        absent("settings-sync-section")
    }

    @Test fun rosterLoadFailureHostedPlayShowsSubscriptionButNoVaultDelete() {
        render(status(HOSTED, keysReady = true), roster = null)

        compose.waitUntil(5_000) { exists("settings-account-offline") }
        compose.onNodeWithTag("settings-subscription-row").fetchSemanticsNode()
        absent("vault-delete-row")
    }

    @Test fun eraseConfirmationEnablesDeleteOnlyForExactErase() {
        render(status(HOSTED, keysReady = true), roster("owner"))
        compose.waitUntil(5_000) { exists("vault-delete-row-button") }

        compose.onNodeWithTag("vault-delete-row-button").performScrollTo().performClick()
        compose.waitUntil(5_000) { exists("vault-delete-final-button") }
        compose.onNodeWithTag("vault-delete-dialog").fetchSemanticsNode()
        compose.onNodeWithTag("vault-delete-final-button").assertIsNotEnabled()
        compose.onNodeWithTag("vault-delete-confirm-field").performTextReplacement("")
        compose.onNodeWithTag("vault-delete-final-button").assertIsNotEnabled()
        compose.onNodeWithTag("vault-delete-confirm-field").performTextReplacement("erase")
        compose.onNodeWithTag("vault-delete-final-button").assertIsNotEnabled()
        compose.onNodeWithTag("vault-delete-confirm-field").performTextReplacement("ERASE")
        compose.onNodeWithTag("vault-delete-final-button").assertIsEnabled()
    }

    @Test fun lockingWhileEraseDialogIsOpenDismissesItAndBlocksDelete() {
        val currentStatus = AtomicReference(status(HOSTED, keysReady = true))
        val deletes = AtomicInteger()
        render(
            status = currentStatus.get(),
            roster = roster("owner"),
            statusSource = { currentStatus.get() },
            vaultDeleter = { deletes.incrementAndGet(); true },
        )
        compose.waitUntil(5_000) { exists("vault-delete-row-button") }
        compose.onNodeWithTag("vault-delete-row-button").performScrollTo().performClick()
        compose.waitUntil(5_000) { exists("vault-delete-final-button") }
        compose.onNodeWithTag("vault-delete-confirm-field").performTextReplacement("ERASE")
        compose.onNodeWithTag("vault-delete-final-button").assertIsEnabled()

        currentStatus.set(status(HOSTED, keysReady = false))

        compose.waitUntil(5_000) { !exists("vault-delete-dialog") }
        assertEquals(0, deletes.get())
    }

    @Test fun vaultDeleteSucceedingAfterLockingReportsChangedOnce() {
        val deferred = CompletableDeferred<Boolean>()
        val calls = AtomicInteger()
        val changed = AtomicInteger()

        confirmVaultDeleteThenLock(deferred, calls) { changed.incrementAndGet() }
        deferred.complete(true)

        compose.waitUntil(5_000) { changed.get() == 1 }
        compose.waitUntil(5_000) { !exists("vault-delete-dialog") }
        assertEquals(1, calls.get())
        assertEquals(1, changed.get())
    }

    @Test fun vaultDeleteFailingAfterLockingShowsFailureOnce() {
        val deferred = CompletableDeferred<Boolean>()
        val calls = AtomicInteger()

        confirmVaultDeleteThenLock(deferred, calls) {}
        deferred.complete(false)

        compose.waitUntil(5_000) { exists("account-error") }
        compose.onNodeWithTag("account-error").assertTextContains(context.getString(R.string.peppy_vault_delete_failed))
        assertEquals(1, calls.get())
    }

    /** Confirms ERASE as a self-hosted owner, then locks while the suspended delete is in flight. */
    private fun confirmVaultDeleteThenLock(deferred: CompletableDeferred<Boolean>, calls: AtomicInteger, onChanged: () -> Unit) {
        val currentStatus = AtomicReference(status(SELF_HOSTED, keysReady = true))
        render(
            status = currentStatus.get(),
            roster = roster("owner"),
            statusSource = { currentStatus.get() },
            vaultDeleter = { calls.incrementAndGet(); runBlocking { deferred.await() } },
            onChanged = onChanged,
        )
        compose.waitUntil(5_000) { exists("vault-delete-row-button") }
        compose.onNodeWithTag("vault-delete-row-button").performScrollTo().performClick()
        compose.waitUntil(5_000) { exists("vault-delete-final-button") }
        compose.onNodeWithTag("vault-delete-confirm-field").performTextReplacement("ERASE")
        compose.onNodeWithTag("vault-delete-final-button").performClick()
        compose.waitUntil(5_000) { calls.get() == 1 }

        currentStatus.set(status(SELF_HOSTED, keysReady = false))
        compose.waitUntil(5_000) { !exists("vault-delete-row") }
    }

    @Test fun accountDeletionRowOpensTheConsentSheetWithTodaysTitle() {
        render(status(HOSTED, keysReady = true), roster("owner"))
        compose.waitUntil(5_000) { exists("account-delete-row-button") }

        compose.onNodeWithTag("account-delete-row-button").performScrollTo().performClick()

        compose.waitUntil(5_000) { exists("account-delete-sheet") }
        assertTextShown(context.getString(R.string.peppy_account_delete_title))
    }

    @Test fun advancedPageShowsServerDiagnosticsAndNoRelayForHostedOrigin() {
        render(status(HOSTED, keysReady = true), roster("owner"))
        compose.waitUntil(5_000) { exists("settings-advanced-row") }

        compose.onNodeWithTag("settings-advanced-row").performScrollTo().performClick()

        compose.waitUntil(5_000) { exists("settings-server") }
        compose.onNodeWithTag("settings-server").assertTextContains(HOSTED, substring = true)
        compose.onNodeWithTag("diagnostics-section").fetchSemanticsNode()
        absent("settings-relay-section")
        compose.onNodeWithTag("settings-advanced-back").performScrollTo().performClick()
        compose.waitUntil(5_000) { exists("settings-advanced-row") }
    }

    @Test fun grantingNotificationPermissionRemovesTheRowAfterThePoll() {
        render(status(SELF_HOSTED, keysReady = true), roster("gateway"))
        compose.waitUntil(5_000) { exists("remote-messaging-notification-permission") }

        shadowOf(ApplicationProvider.getApplicationContext<Application>()).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)

        compose.waitUntil(5_000) { !exists("remote-messaging-notification-permission") }
    }

    @Test fun notificationRowOpensTheExistingConsentSheet() {
        render(status(SELF_HOSTED, keysReady = true), roster("gateway"))
        compose.waitUntil(5_000) { exists("remote-messaging-notification-permission-button") }

        compose.onNodeWithTag("remote-messaging-notification-permission-button").performClick()

        compose.onNodeWithTag("remote-messaging-notification-consent").fetchSemanticsNode()
        assertTextShown(context.getString(R.string.peppy_toggle_android_settings_notifications_title))
    }

    @Test fun hostedDeletionRowOpensTheSheetWithTodaysTitle() {
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionRow(tag = "account-delete-row", onOpenFailed = {}, isPlay = false)
            }
        }

        compose.onNodeWithTag("account-delete-row-button").performClick()

        compose.waitUntil(5_000) { exists("account-delete-sheet") }
        assertTextShown(context.getString(R.string.peppy_account_delete_title))
    }

    @Test fun playManagedDeletionRowOffersCancelInPlay() {
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionRow(
                    tag = "account-delete-row",
                    onOpenFailed = {},
                    isPlay = true,
                    playManaged = true,
                    playProductId = "peppy_monthly",
                )
            }
        }

        compose.onNodeWithTag("account-delete-row-button").performClick()

        compose.onNodeWithTag("account-delete-play-cancel").fetchSemanticsNode()
    }

    private fun render(
        status: GatewayStatus,
        roster: GatewayAccountState?,
        isPlay: Boolean = true,
        playDeletionContext: PlayDeletionContext = PlayDeletionContext(false, null),
        statusSource: () -> GatewayStatus = { status },
        vaultDeleter: (Context) -> Boolean = { false },
        onChanged: () -> Unit = {},
    ) {
        compose.setContent {
            MaterialTheme {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    SettingsScreen(
                        status = status,
                        isPlay = isPlay,
                        playDeletionContext = playDeletionContext,
                        onClose = {},
                        onOpenSubscription = {},
                        onChanged = onChanged,
                        accountLoader = { roster },
                        gatewayStatusLoader = { statusSource() },
                        vaultDeleter = vaultDeleter,
                    )
                }
            }
        }
    }

    private fun status(origin: String, keysReady: Boolean) = GatewayStatus(
        enrolled = true,
        origin = origin,
        databaseOpen = true,
        sharedKeysReady = keysReady,
        syncPhase = SyncPhase.LIVE,
    )

    private fun roster(role: String) = GatewayAccountState(
        self = GatewayAccountDevice(id = "self", role = role, revoked = false, keyEpoch = 1),
        devices = listOf(GatewayAccountDevice(id = "self", role = role, revoked = false, keyEpoch = 1)),
    )

    private fun relaySummary() = context.getString(R.string.peppy_settings_advanced_summary_relay)

    private fun exists(tag: String) = compose.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()

    private fun absent(tag: String) = compose.onAllNodesWithTag(tag).assertCountEquals(0)

    private fun assertTextShown(text: String) {
        check(compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()) { "Text not shown: $text" }
    }
}

private const val HOSTED = "https://peppy.pro"
private const val SELF_HOSTED = "https://gateway.example.test"
