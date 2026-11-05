package dev.peppy.mobile.enrollment

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performSemanticsAction
import dev.peppy.mobile.account.HostedAccountDeletionEntry
import dev.peppy.mobile.account.PLAY_SUBSCRIPTIONS_URL
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.MutableSharedFlow

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class HostedBillingAndDeletionComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun playBillingPhaseDoesNotComposeDirectWebsiteHooks() {
        compose.setContent {
            MaterialTheme {
                HostedBillingPhase(isPlay = true, playContent = {
                    Text("Play", Modifier.testTag("play-billing-content"))
                    HostedPlayEnrollmentCheck {}
                }, onSubscribe = {}, onCheckAgain = {})
            }
        }

        compose.onNodeWithTag("play-billing-content").fetchSemanticsNode()
        compose.onNodeWithTag("play-enrollment-check-again").fetchSemanticsNode()
        compose.onNodeWithTag("hosted-billing-body").assertDoesNotExist()
        compose.onNodeWithTag("hosted-subscribe-button").assertDoesNotExist()
        compose.onNodeWithTag("hosted-billing-check-again").assertDoesNotExist()
    }

    @Test fun directBillingPhaseRetainsWebsiteActions() {
        val subscriptions = AtomicInteger()
        val checks = AtomicInteger()
        compose.setContent {
            MaterialTheme { HostedBillingPhase(isPlay = false, playContent = {}, onSubscribe = { subscriptions.incrementAndGet() }, onCheckAgain = { checks.incrementAndGet() }) }
        }

        compose.onNodeWithTag("hosted-billing-body").fetchSemanticsNode()
        compose.onNodeWithTag("hosted-subscribe-button").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("hosted-billing-check-again").performSemanticsAction(SemanticsActions.OnClick)
        compose.runOnIdle {
            assertEquals(1, subscriptions.get())
            assertEquals(1, checks.get())
        }
    }

    @Test fun deletionBrowserFailureKeepsCurrentPhaseContentAndShowsInlineError() {
        compose.setContent {
            MaterialTheme {
                Column {
                    Text("Passphrase", Modifier.testTag("passphrase-phase-content"))
                    HostedEnrollmentDeletionEntry(openDeletion = { false })
                }
            }
        }

        compose.onNodeWithTag("hosted-delete-account-button").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-confirm").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("passphrase-phase-content").fetchSemanticsNode()
        compose.onNodeWithTag("hosted-delete-account-error").fetchSemanticsNode()
    }

    @Test fun deletionManagementActionIsPlayOnly() {
        val managementActions = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(tag = "delete-play", onOpenFailed = {}, onViewSubscription = { managementActions.incrementAndGet() }, isPlay = true)
            }
        }
        compose.onNodeWithTag("delete-play").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").performSemanticsAction(SemanticsActions.OnClick)
        compose.runOnIdle { assertEquals(1, managementActions.get()) }
    }

    @Test fun directDeletionDoesNotComposeManagementAction() {
        val managementActions = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(tag = "delete-direct", onOpenFailed = {}, onViewSubscription = { managementActions.incrementAndGet() }, isPlay = false)
            }
        }
        compose.onNodeWithTag("delete-direct").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").assertDoesNotExist()
    }

    @Test fun playDeletionWithoutCallbackOpensTrustedSubscriptionListAndClosesSheet() {
        val opened = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(
                    tag = "delete-play-default",
                    onOpenFailed = {},
                    isPlay = true,
                    openPlaySubscriptions = { opened.incrementAndGet(); true },
                )
            }
        }
        compose.onNodeWithTag("delete-play-default").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").assertDoesNotExist()
        compose.runOnIdle {
            assertEquals(1, opened.get())
            assertEquals("https://play.google.com/store/account/subscriptions", PLAY_SUBSCRIPTIONS_URL)
        }
    }

    @Test fun playDeletionBrowserNotFoundReportsInlineFailureAndClosesSheet() {
        val failures = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(
                    tag = "delete-play-no-browser",
                    onOpenFailed = { failures.incrementAndGet() },
                    isPlay = true,
                    openPlaySubscriptions = { false },
                )
            }
        }
        compose.onNodeWithTag("delete-play-no-browser").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").assertDoesNotExist()
        compose.runOnIdle { assertEquals(1, failures.get()) }
    }

    @Test fun explicitPlayManagementCallbackOverridesDefaultSubscriptionList() {
        val managementActions = AtomicInteger()
        val defaultOpens = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(
                    tag = "delete-play-callback",
                    onOpenFailed = {},
                    onViewSubscription = { managementActions.incrementAndGet() },
                    isPlay = true,
                    openPlaySubscriptions = { defaultOpens.incrementAndGet(); true },
                )
            }
        }
        compose.onNodeWithTag("delete-play-callback").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").performSemanticsAction(SemanticsActions.OnClick)
        compose.runOnIdle {
            assertEquals(1, managementActions.get())
            assertEquals(0, defaultOpens.get())
        }
    }

    @Test fun directDeletionWithoutCallbackHasNoSubscriptionActionAndOpensNothingExtra() {
        val defaultOpens = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedAccountDeletionEntry(
                    tag = "delete-direct-default",
                    onOpenFailed = {},
                    isPlay = false,
                    openPlaySubscriptions = { defaultOpens.incrementAndGet(); true },
                )
            }
        }
        compose.onNodeWithTag("delete-direct-default").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("account-delete-sheet-view-subscription").assertDoesNotExist()
        compose.runOnIdle { assertEquals(0, defaultOpens.get()) }
    }

    @Test fun linkedEventFromBillingAdvancesReadWriteAccountToPassphraseOnce() {
        val linked = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val reconciliations = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                var phase by remember { mutableStateOf<HostedPhase>(HostedPhase.Billing) }
                val accountAccess = "read_write"
                HostedEnrollmentLinkReconciler(linked, { phase }) {
                    reconciliations.incrementAndGet()
                    if (accountAccess == "read_write") phase = HostedPhase.Passphrase
                }
                Text(phase.toString(), Modifier.testTag("enrollment-phase-${phase::class.simpleName}"))
            }
        }

        compose.runOnIdle { linked.tryEmit(Unit) }
        compose.waitForIdle()
        compose.onNodeWithTag("enrollment-phase-Passphrase").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(1, reconciliations.get()) }
    }

    @Test fun providerStateWithoutLinkedEventDoesNotReconcile() {
        val linked = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
        val reconciliations = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                HostedEnrollmentLinkReconciler(linked, { HostedPhase.Billing }) { reconciliations.incrementAndGet() }
                Text("Billed through Google Play", Modifier.testTag("play-subscription-provider"))
            }
        }

        compose.waitForIdle()
        compose.onNodeWithTag("play-subscription-provider").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(0, reconciliations.get()) }
    }

    @Test fun manualPlayCheckLeavesReadOnlyBillingWithoutAutomaticLoop() {
        val reconciliations = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                var phase by remember { mutableStateOf<HostedPhase>(HostedPhase.Billing) }
                HostedPlayEnrollmentCheck {
                    reconciliations.incrementAndGet()
                    phase = HostedPhase.Billing
                }
                Text(phase.toString(), Modifier.testTag("enrollment-phase-${phase::class.simpleName}"))
            }
        }

        compose.onNodeWithTag("play-enrollment-check-again").performSemanticsAction(SemanticsActions.OnClick)
        compose.waitForIdle()
        compose.onNodeWithTag("enrollment-phase-Billing").fetchSemanticsNode()
        compose.runOnIdle { assertEquals(1, reconciliations.get()) }
    }
}
