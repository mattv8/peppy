package dev.peppy.mobile.billing

import android.app.Activity
import android.app.Application
import android.content.Context
import android.content.Intent
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.account.openHostedAccount
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import uniffi.peppy_mobile_bindings.NativeBillingProvider
import uniffi.peppy_mobile_bindings.NativePlayBillingContext
import uniffi.peppy_mobile_bindings.NativePlayLinkOutcome

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PlaySubscriptionPanelComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun sameAccountReauthenticationRevisionTriggersOneAdditionalLoad() {
        val loads = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                var revision by remember { mutableIntStateOf(0) }
                PlaySubscriptionLoadEffect("account", "vault", revision) { _, _ -> loads.incrementAndGet() }
                LaunchedEffect(Unit) { revision = 1 }
            }
        }

        compose.waitForIdle()
        compose.runOnIdle { assertEquals(2, loads.get()) }
    }

    @Test fun providerPlayHidesRestoreWhenEnrollmentSuppliesSingleCheckAction() {
        compose.setContent {
            MaterialTheme {
                ProviderPlay(
                    context = NativePlayBillingContext("account", true, "peppy.monthly", listOf("monthly"), "binding", NativeBillingProvider.PLAY),
                    notice = null,
                    androidContext = LocalContext.current,
                    onRestore = {},
                    showRestoreActions = false,
                )
            }
        }

        compose.onNodeWithTag("play-manage-button").fetchSemanticsNode()
        compose.onNodeWithTag("play-restore-button").assertDoesNotExist()
    }

    @Test fun enrollmentReadyPanelHidesRestoreAction() {
        composePanel(panelCoordinator())

        compose.onNodeWithTag("play-restore-button").assertDoesNotExist()
        compose.onNodeWithTag("play-pending-check").assertDoesNotExist()
    }

    @Test fun accountPanelKeepsRestoreActionByDefault() {
        compose.setContent {
            MaterialTheme {
                PlaySubscriptionPanel(panelCoordinator(), "account", onSignIn = {})
            }
        }

        compose.onNodeWithTag("play-restore-button").fetchSemanticsNode()
    }

    @Test fun enrollmentNotEntitlingPanelHidesRestoreAction() {
        composePanel(panelCoordinator(purchases = listOf(panelPurchase()), outcome = NativePlayLinkOutcome.NOT_ENTITLING))

        compose.onNodeWithTag("play-restore-button").assertDoesNotExist()
        compose.onNodeWithTag("play-pending-check").assertDoesNotExist()
    }

    @Test fun enrollmentPendingPanelHidesCheckAgainAction() {
        composePanel(panelCoordinator(purchases = listOf(panelPurchase(pending = true))))

        compose.onNodeWithTag("play-restore-button").assertDoesNotExist()
        compose.onNodeWithTag("play-pending-check").assertDoesNotExist()
    }

    private fun composePanel(coordinator: PlayBillingCoordinator) {
        compose.setContent {
            MaterialTheme {
                PlaySubscriptionPanel(coordinator, "account", onSignIn = {}, showRestoreActions = false)
            }
        }
    }

    private fun composeAccountPanel(coordinator: PlayBillingCoordinator, openAccountWebsite: ((Context) -> Boolean)?) {
        compose.setContent {
            MaterialTheme {
                PlaySubscriptionPanel(coordinator, "account", onSignIn = {}, openAccountWebsite = openAccountWebsite)
            }
        }
    }

    @Test fun websiteBilledAccountPageOpensTheHostedAccountPage() {
        composeAccountPanel(panelCoordinator(provider = NativeBillingProvider.STRIPE), ::openHostedAccount)

        compose.onNodeWithTag("play-subscription-status").fetchSemanticsNode()
        compose.onNodeWithTag("play-account-website-link").performClick()

        val started = shadowOf(ApplicationProvider.getApplicationContext<Application>()).nextStartedActivity
        assertEquals(Intent.ACTION_VIEW, started.action)
        assertEquals("https://peppy.pro/account", started.dataString)
        compose.onNodeWithTag("play-account-website-error").assertDoesNotExist()
    }

    @Test fun websiteBilledEnrollmentPanelOffersNoWebsiteLink() {
        composePanel(panelCoordinator(provider = NativeBillingProvider.STRIPE))

        compose.onNodeWithTag("play-subscription-status").fetchSemanticsNode()
        compose.onNodeWithTag("play-account-website").assertDoesNotExist()
    }

    @Test fun websiteLinkShowsInlineFailureUntilAnOpenSucceeds() {
        val attempts = AtomicInteger()
        composeAccountPanel(panelCoordinator(provider = NativeBillingProvider.STRIPE)) { attempts.incrementAndGet() > 1 }

        compose.onNodeWithTag("play-account-website-link").performClick()
        compose.onNodeWithTag("play-account-website-error").fetchSemanticsNode()
        compose.onNodeWithTag("play-account-website-link").performClick()
        compose.onNodeWithTag("play-account-website-error").assertDoesNotExist()
        compose.runOnIdle { assertEquals(2, attempts.get()) }
    }

    @Test fun purchasablePanelNeverOffersWebsiteLink() {
        composeAccountPanel(panelCoordinator()) { true }

        compose.onNodeWithTag("play-subscribe-button").fetchSemanticsNode()
        compose.onNodeWithTag("play-account-website").assertDoesNotExist()
    }

    private fun panelCoordinator(
        purchases: List<BillingPurchase> = emptyList(),
        outcome: NativePlayLinkOutcome = NativePlayLinkOutcome.LINKED,
        provider: NativeBillingProvider = NativeBillingProvider.NONE,
    ): PlayBillingCoordinator {
        val snapshot = BillingSessionSnapshot("account", "session", null)
        val purchasable = provider == NativeBillingProvider.NONE
        val context = NativePlayBillingContext("account", purchasable, "peppy.monthly", listOf("monthly"), "binding", provider)
        return PlayBillingCoordinator(
            PanelHost(snapshot, context, outcome),
            PanelGateway(purchases),
            CoroutineScope(Job() + Dispatchers.Unconfined),
            refreshAccount = {},
        )
    }

    private class PanelHost(
        private val snapshot: BillingSessionSnapshot,
        private val context: NativePlayBillingContext,
        private val outcome: NativePlayLinkOutcome,
    ) : PlayBillingHost {
        override suspend fun snapshot(accountId: String) = snapshot
        override suspend fun context(snapshot: BillingSessionSnapshot) = context
        override suspend fun link(
            snapshot: BillingSessionSnapshot,
            operationId: String,
            productId: String,
            token: String,
        ) = outcome
        override suspend fun current(snapshot: BillingSessionSnapshot) = true
    }

    private class PanelGateway(private val purchases: List<BillingPurchase>) : PlayBillingGateway {
        override fun setEvents(listener: (Int, List<BillingPurchase>) -> Unit) = Unit
        override fun connect(onReady: (Boolean) -> Unit) = onReady(true)
        override fun queryOffers(productId: String, onResult: (Boolean, List<BillingOffer>) -> Unit) =
            onResult(true, listOf(BillingOffer(productId, "monthly", "offer", "$9.99", false)))
        override fun launch(activity: Activity, offer: BillingOffer, accountBinding: String) = 0
        override fun queryPurchases(onResult: (Boolean, List<BillingPurchase>) -> Unit) = onResult(true, purchases)
        override fun close() = Unit
    }

    private companion object {
        fun panelPurchase(pending: Boolean = false) = BillingPurchase("token", listOf("peppy.monthly"), "binding", pending)
    }
}
