package dev.peppy.mobile.billing

import android.app.Activity
import com.android.billingclient.api.BillingClient
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeBillingProvider
import uniffi.peppy_mobile_bindings.NativePlayBillingContext
import uniffi.peppy_mobile_bindings.NativePlayLinkOutcome

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PlayBillingTest {
    private val activity: Activity = Robolectric.buildActivity(Activity::class.java).get()

    @Test fun acceptsOnlyBoundedPlayProductIds() {
        assertTrue(isValidatedPlayProductId("peppy.monthly"))
        assertFalse(isValidatedPlayProductId("bad product"))
        assertFalse(isValidatedPlayProductId("x".repeat(129)))
    }

    @Test fun managementUriIsLocallyConstructedOnlyForPlayProvider() {
        val context = billingContext(provider = NativeBillingProvider.PLAY)
        assertEquals("https://play.google.com/store/account/subscriptions?sku=peppy.monthly&package=dev.peppy.mobile", playManageUri(context).toString())
        assertNull(playManageUri(context.copy(currentProvider = NativeBillingProvider.STRIPE)))
        assertNull(playManageUri(context.copy(productId = "not valid")))
    }

    @Test fun initialLoadReachesReadyAndStartsAutomaticRestore() {
        val fixture = fixture()
        fixture.coordinator.load("account")
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Ready)
        assertEquals(1, fixture.gateway.purchaseQueries)
    }

    @Test fun resumeHonorsProviderAndVaultGates() {
        listOf(NativeBillingProvider.STRIPE, NativeBillingProvider.UNKNOWN).forEach { provider ->
            val fixture = fixture(context = billingContext(provider = provider))
            fixture.coordinator.load("account")
            fixture.coordinator.onResume()
            assertEquals(0, fixture.gateway.connectCalls)
            assertEquals(0, fixture.gateway.purchaseQueries)
        }
        val mismatch = fixture(snapshot = snapshot(vaultId = "account-vault"))
        mismatch.coordinator.load("account", "device-vault")
        assertTrue(mismatch.coordinator.state.value is PlaySubscriptionState.VaultMismatch)
        mismatch.coordinator.onResume()
        assertEquals(0, mismatch.gateway.connectCalls)
        assertEquals(0, mismatch.gateway.purchaseQueries)

        val play = fixture(context = billingContext(provider = NativeBillingProvider.PLAY))
        play.coordinator.load("account")
        play.coordinator.onResume()
        assertEquals(1, play.gateway.connectCalls)
        play.gateway.completeConnect()
        assertEquals(1, play.gateway.purchaseQueries)
    }

    @Test fun acknowledgedAndUnacknowledgedPurchasedTokensAreBothRestored() {
        val fixture = readyFixture()
        var linkedEvents = 0
        val collector = fixture.scope.launch { fixture.coordinator.linked.collect { linkedEvents++ } }
        fixture.gateway.completePurchases(listOf(purchase("acknowledged-token"), purchase("unacknowledged-token")))
        assertEquals(listOf("acknowledged-token", "unacknowledged-token"), fixture.host.linkedTokens)
        assertEquals(2, linkedEvents)
        collector.cancel()
    }

    @Test fun pendingAndForeignPurchasesNeverLink() {
        val fixture = readyFixture()
        fixture.gateway.completePurchases(listOf(
            purchase("pending", pending = true),
            purchase("wrong-product", products = listOf("other")),
            purchase("multiple-products", products = listOf("peppy.monthly", "other")),
            purchase("missing-binding", binding = null),
            purchase("wrong-binding", binding = "other-binding"),
        ))
        assertTrue(fixture.host.linkedTokens.isEmpty())
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Pending)
    }

    @Test fun purchaseCallbackUsesCapturedLaunchIdentityAndRejectsStaleSession() {
        val fixture = readyFixture(snapshot = snapshot(fence = "session-one"))
        fixture.coordinator.purchase(activity)
        fixture.host.snapshot = snapshot(fence = "session-two")
        fixture.coordinator.load("account")
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(purchase("old-callback")))
        assertTrue(fixture.host.linkedTokens.isEmpty())
    }

    @Test fun callbacksRejectWrongProductAndBinding() {
        val fixture = readyFixture()
        fixture.coordinator.purchase(activity)
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(
            purchase("wrong-product", products = listOf("other")),
            purchase("missing-binding", binding = null),
            purchase("wrong-binding", binding = "other-binding"),
        ))
        assertTrue(fixture.host.linkedTokens.isEmpty())
    }

    @Test fun lateOfferAndPurchaseResultsAreFencedAfterAccountChange() {
        val fixture = fixture()
        fixture.coordinator.load("account")
        fixture.gateway.completeConnect()
        fixture.host.snapshot = snapshot(accountId = "other", fence = "other-session")
        fixture.host.context = billingContext(accountId = "other")
        fixture.coordinator.load("other")
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Loading)

        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        fixture.coordinator.restore()
        fixture.coordinator.load(null)
        fixture.gateway.completePurchases(listOf(purchase("late")))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.SignInRequired)
        assertTrue(fixture.host.linkedTokens.isEmpty())
    }

    @Test fun lateAutomaticRestoreDoesNotOverwritePurchasing() {
        val fixture = readyFixture()
        fixture.coordinator.purchase(activity)
        fixture.gateway.completePurchases(emptyList())
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Purchasing)
    }

    @Test fun userCancellationReturnsToReadyWithTheSameOffers() {
        val fixture = readyFixture()
        val ready = fixture.coordinator.state.value as PlaySubscriptionState.Ready
        fixture.coordinator.purchase(activity)
        fixture.gateway.emit(BillingClient.BillingResponseCode.USER_CANCELED, emptyList())
        assertEquals(ready, fixture.coordinator.state.value)
    }

    @Test fun itemAlreadyOwnedQueriesExistingPurchases() {
        val fixture = readyFixture()
        fixture.gateway.launchResult = BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED
        fixture.coordinator.purchase(activity)
        assertEquals(2, fixture.gateway.purchaseQueries)
    }

    @Test fun expiredSessionDuringLinkRequiresSignIn() {
        val fixture = readyFixture()
        fixture.host.linkFailure = dev.peppy.mobile.account.HostedFailure(
            dev.peppy.mobile.account.HostedFailure.Kind.SESSION_EXPIRED,
        )
        fixture.gateway.completePurchases(listOf(purchase("expired-link")))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.SignInRequired)
    }

    @Test fun billingConnectionFailureCanBeRetried() {
        val fixture = fixture()
        fixture.coordinator.load("account")
        fixture.gateway.completeConnect(ready = false)
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.BillingUnavailable)

        fixture.coordinator.retry()
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Ready)
    }

    @Test fun transientLinkFailureCanBeRetriedWithTheSameToken() {
        val fixture = readyFixture()
        fixture.host.linkFailure = IllegalStateException("offline")
        fixture.gateway.completePurchases(listOf(purchase("retry-token")))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.LinkRetry)
        fixture.host.linkFailure = null
        fixture.coordinator.restore()
        fixture.gateway.completePurchases(listOf(purchase("retry-token")))
        assertEquals(listOf("retry-token", "retry-token"), fixture.host.linkedTokens)
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.ProviderPlay)
    }

    @Test fun manualAndAutomaticEmptyRestorePreserveBaseState() {
        val fixture = readyFixture()
        val ready = fixture.coordinator.state.value
        fixture.gateway.completePurchases(emptyList())
        assertEquals(ready, fixture.coordinator.state.value)
        assertNull(fixture.coordinator.notice.value)
        fixture.coordinator.restore()
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.RestoreWorking)
        fixture.gateway.completePurchases(emptyList())
        assertEquals(ready, fixture.coordinator.state.value)
        assertEquals(PlayBillingNotice.RESTORE_NONE, fixture.coordinator.notice.value)
    }

    @Test fun retryUsesRequestedAccountAndVaultAfterInitialSnapshotNetworkFailure() {
        val fixture = fixture(snapshot = snapshot(vaultId = "device-vault"))
        fixture.host.snapshotFailure = dev.peppy.mobile.account.HostedFailure(
            dev.peppy.mobile.account.HostedFailure.Kind.UNAVAILABLE,
        )
        fixture.coordinator.load("account", "device-vault")
        assertEquals(PlaySubscriptionState.Error(PlayBillingError.NETWORK), fixture.coordinator.state.value)

        fixture.host.snapshotFailure = null
        fixture.coordinator.retry()
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))

        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Ready)
    }

    @Test fun expiredSessionAndNetworkFailuresHaveDifferentStates() {
        val expired = fixture()
        expired.host.snapshotFailure = dev.peppy.mobile.account.HostedFailure(dev.peppy.mobile.account.HostedFailure.Kind.SESSION_EXPIRED)
        expired.coordinator.load("account")
        assertTrue(expired.coordinator.state.value is PlaySubscriptionState.SignInRequired)
        val network = fixture()
        network.host.snapshotFailure = dev.peppy.mobile.account.HostedFailure(dev.peppy.mobile.account.HostedFailure.Kind.UNAVAILABLE)
        network.coordinator.load("account")
        assertEquals(PlaySubscriptionState.Error(PlayBillingError.NETWORK), network.coordinator.state.value)
    }

    @Test fun automaticEmptyRestorePreservesProviderPlayState() {
        val fixture = fixture(context = billingContext(provider = NativeBillingProvider.PLAY))
        fixture.coordinator.load("account")
        fixture.coordinator.onResume()
        fixture.gateway.completeConnect()
        fixture.gateway.completePurchases(emptyList())
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.ProviderPlay)
    }

    @Test fun mismatchAcknowledgmentIsBoundToExactSessionAndVault() {
        val fixture = fixture(snapshot = snapshot(fence = "one", vaultId = "account-vault"))
        fixture.coordinator.load("account", "device-vault")
        fixture.coordinator.continueWithSignedInAccount()
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.Ready)
        fixture.host.snapshot = snapshot(fence = "two", vaultId = "account-vault")
        fixture.coordinator.load("account", "device-vault")
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.VaultMismatch)
    }

    @Test fun duplicateCallbacksLinkOncePerSessionButNewSessionMayRelink() {
        val fixture = readyFixture()
        fixture.gateway.completePurchases(emptyList())
        fixture.coordinator.purchase(activity)
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(purchase("token"), purchase("token")))
        assertEquals(listOf("token"), fixture.host.linkedTokens)
        fixture.host.snapshot = snapshot(fence = "new-session")
        fixture.coordinator.load("account")
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        fixture.gateway.completePurchases(listOf(purchase("token")))
        assertEquals(listOf("token", "token"), fixture.host.linkedTokens)
    }

    @Test fun lateLinkResultIsDiscardedAfterSessionChange() = runBlocking {
        val deferred = CompletableDeferred<NativePlayLinkOutcome>()
        val fixture = readyFixture()
        fixture.host.deferredLink = deferred
        fixture.gateway.completePurchases(listOf(purchase("late-link")))
        fixture.host.snapshot = snapshot(fence = "replacement")
        fixture.coordinator.load("account")
        deferred.complete(NativePlayLinkOutcome.LINKED)
        assertEquals(0, fixture.refreshes)
        assertFalse(fixture.coordinator.state.value is PlaySubscriptionState.ProviderPlay)
    }

    @Test fun linkedEventEmitsOnceAndDuplicateRestoreEmitsNothing() {
        val fixture = readyFixture()
        var linkedEvents = 0
        val collector = fixture.scope.launch { fixture.coordinator.linked.collect { linkedEvents++ } }
        fixture.gateway.completePurchases(emptyList())
        fixture.coordinator.purchase(activity)
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(purchase("linked-token")))
        assertEquals(1, linkedEvents)

        fixture.coordinator.restore()
        fixture.gateway.completePurchases(listOf(purchase("linked-token")))
        assertEquals(1, linkedEvents)
        collector.cancel()
    }

    @Test fun staleLinkedResultEmitsNothing() = runBlocking {
        val deferred = CompletableDeferred<NativePlayLinkOutcome>()
        val fixture = readyFixture()
        var linkedEvents = 0
        val collector = fixture.scope.launch { fixture.coordinator.linked.collect { linkedEvents++ } }
        fixture.host.deferredLink = deferred
        fixture.gateway.completePurchases(listOf(purchase("stale-linked-token")))
        fixture.host.snapshot = snapshot(fence = "replacement")
        fixture.coordinator.load("account")
        deferred.complete(NativePlayLinkOutcome.LINKED)
        assertEquals(0, linkedEvents)
        collector.cancel()
    }

    @Test fun loadingProviderPlayContextDoesNotEmitLinkedEvent() {
        val fixture = fixture(context = billingContext(provider = NativeBillingProvider.PLAY))
        var linkedEvents = 0
        val collector = fixture.scope.launch { fixture.coordinator.linked.collect { linkedEvents++ } }
        fixture.coordinator.load("account")
        assertTrue(fixture.coordinator.state.value is PlaySubscriptionState.ProviderPlay)
        assertEquals(0, linkedEvents)
        collector.cancel()
    }

    @Test fun successfulLinkRefreshesCanonicalPlayContextForManageAndRestore() {
        val fixture = readyFixture()
        fixture.gateway.completePurchases(emptyList())
        fixture.coordinator.purchase(activity)
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(purchase("linked-token")))

        val providerPlay = fixture.coordinator.state.value as PlaySubscriptionState.ProviderPlay
        assertEquals(NativeBillingProvider.PLAY, providerPlay.context.currentProvider)
        assertTrue(playManageUri(providerPlay.context) != null)
        fixture.coordinator.restore()
        assertEquals(2, fixture.gateway.purchaseQueries)
    }

    @Test fun releaseClosesOnceAndDiscardsLateCallbacks() {
        val fixture = fixture()
        fixture.coordinator.load("account")
        fixture.coordinator.release()
        fixture.coordinator.release()
        fixture.gateway.completeConnect()
        fixture.gateway.emit(BillingClient.BillingResponseCode.OK, listOf(purchase("late")))
        assertEquals(1, fixture.gateway.closeCalls)
        assertTrue(fixture.host.linkedTokens.isEmpty())
    }

    @Test fun purchaseRechecksSessionBeforeLaunching() {
        val fixture = readyFixture()
        fixture.host.current = false
        fixture.coordinator.purchase(activity)
        assertEquals(0, fixture.gateway.launchCalls)
    }

    @Test fun billingPurchaseStringDoesNotExposeToken() {
        assertFalse(purchase("secret-purchase-token").toString().contains("secret-purchase-token"))
    }

    private fun readyFixture(snapshot: BillingSessionSnapshot = snapshot()): Fixture {
        val fixture = fixture(snapshot = snapshot)
        fixture.coordinator.load(snapshot.accountId)
        fixture.gateway.completeConnect()
        fixture.gateway.completeOffers(listOf(monthlyOffer()))
        return fixture
    }

    private fun fixture(snapshot: BillingSessionSnapshot = snapshot(), context: NativePlayBillingContext = billingContext(accountId = snapshot.accountId)): Fixture {
        val host = FakeHost(snapshot, context)
        val gateway = FakeGateway()
        var refreshes = 0
        val scope = CoroutineScope(Job() + Dispatchers.Unconfined)
        val coordinator = PlayBillingCoordinator(host, gateway, scope) { refreshes++ }
        return Fixture(host, gateway, coordinator, scope) { refreshes }
    }

    private data class Fixture(
        val host: FakeHost,
        val gateway: FakeGateway,
        val coordinator: PlayBillingCoordinator,
        val scope: CoroutineScope,
        private val refreshCount: () -> Int,
    ) {
        val refreshes get() = refreshCount()
    }

    private class FakeHost(var snapshot: BillingSessionSnapshot, var context: NativePlayBillingContext) : PlayBillingHost {
        var current = true
        var snapshotFailure: Throwable? = null
        var linkFailure: Throwable? = null
        var deferredLink: CompletableDeferred<NativePlayLinkOutcome>? = null
        val linkedTokens = mutableListOf<String>()
        override suspend fun snapshot(accountId: String): BillingSessionSnapshot { snapshotFailure?.let { throw it }; return snapshot }
        override suspend fun context(snapshot: BillingSessionSnapshot): NativePlayBillingContext = context
        override suspend fun link(snapshot: BillingSessionSnapshot, operationId: String, productId: String, token: String): NativePlayLinkOutcome {
            linkedTokens += token
            linkFailure?.let { throw it }
            return deferredLink?.await() ?: NativePlayLinkOutcome.LINKED
        }
        override suspend fun current(snapshot: BillingSessionSnapshot): Boolean = current && this.snapshot.fence == snapshot.fence
    }

    private class FakeGateway : PlayBillingGateway {
        private var events: (Int, List<BillingPurchase>) -> Unit = { _, _ -> }
        private val connections = ArrayDeque<(Boolean) -> Unit>()
        private val offerResults = ArrayDeque<(Boolean, List<BillingOffer>) -> Unit>()
        private val purchaseResults = ArrayDeque<(Boolean, List<BillingPurchase>) -> Unit>()
        var connectCalls = 0
        var purchaseQueries = 0
        var launchCalls = 0
        var closeCalls = 0
        var launchResult = BillingClient.BillingResponseCode.OK
        override fun setEvents(listener: (Int, List<BillingPurchase>) -> Unit) { events = listener }
        override fun connect(onReady: (Boolean) -> Unit) { connectCalls++; connections += onReady }
        override fun queryOffers(productId: String, onResult: (Boolean, List<BillingOffer>) -> Unit) { offerResults += onResult }
        override fun launch(activity: Activity, offer: BillingOffer, accountBinding: String): Int { launchCalls++; return launchResult }
        override fun queryPurchases(onResult: (Boolean, List<BillingPurchase>) -> Unit) { purchaseQueries++; purchaseResults += onResult }
        override fun close() { closeCalls++ }
        fun completeConnect(ready: Boolean = true) = connections.removeFirst().invoke(ready)
        fun completeOffers(offers: List<BillingOffer>, ok: Boolean = true) = offerResults.removeFirst().invoke(ok, offers)
        fun completePurchases(purchases: List<BillingPurchase>, ok: Boolean = true) = purchaseResults.removeFirst().invoke(ok, purchases)
        fun emit(code: Int, purchases: List<BillingPurchase>) = events(code, purchases)
    }

    private companion object {
        fun snapshot(accountId: String = "account", fence: String = "session", vaultId: String? = null) = BillingSessionSnapshot(accountId, fence, vaultId)
        fun billingContext(accountId: String = "account", provider: NativeBillingProvider = NativeBillingProvider.NONE) = NativePlayBillingContext(accountId, true, "peppy.monthly", listOf("monthly", "annual"), "binding", provider)
        fun monthlyOffer() = BillingOffer("peppy.monthly", "monthly", "offer", "$9.99", false)
        fun purchase(token: String, products: List<String> = listOf("peppy.monthly"), binding: String? = "binding", pending: Boolean = false) = BillingPurchase(token, products, binding, pending)
    }
}
