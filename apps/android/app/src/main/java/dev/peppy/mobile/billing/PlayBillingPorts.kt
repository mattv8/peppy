package dev.peppy.mobile.billing

import android.app.Activity
import android.content.Context
import com.android.billingclient.api.BillingClient
import com.android.billingclient.api.BillingClientStateListener
import com.android.billingclient.api.BillingFlowParams
import com.android.billingclient.api.Purchase
import com.android.billingclient.api.PurchasesUpdatedListener
import com.android.billingclient.api.QueryProductDetailsParams
import com.android.billingclient.api.QueryPurchasesParams
import dev.peppy.mobile.account.HostedAccountClient
import uniffi.peppy_mobile_bindings.NativePlayBillingContext
import uniffi.peppy_mobile_bindings.NativePlayLinkOutcome

internal data class BillingSessionSnapshot(val accountId: String, val fence: String, val vaultId: String?)
internal data class BillingOffer(val productId: String, val basePlanId: String, val offerToken: String, val price: String, val annual: Boolean)
internal class BillingPurchase(
    val token: String,
    val products: List<String>,
    val obfuscatedAccountId: String?,
    val pending: Boolean,
) {
    override fun toString() =
        "BillingPurchase(token=[redacted], products=$products, obfuscatedAccountId=$obfuscatedAccountId, pending=$pending)"
}
internal interface PlayBillingHost {
    suspend fun snapshot(accountId: String): BillingSessionSnapshot
    suspend fun context(snapshot: BillingSessionSnapshot): NativePlayBillingContext
    suspend fun link(snapshot: BillingSessionSnapshot, operationId: String, productId: String, token: String): NativePlayLinkOutcome
    suspend fun current(snapshot: BillingSessionSnapshot): Boolean
}
internal interface PlayBillingGateway {
    fun setEvents(listener: (Int, List<BillingPurchase>) -> Unit)
    fun connect(onReady: (Boolean) -> Unit)
    fun queryOffers(productId: String, onResult: (Boolean, List<BillingOffer>) -> Unit)
    fun launch(activity: Activity, offer: BillingOffer, accountBinding: String): Int
    fun queryPurchases(onResult: (Boolean, List<BillingPurchase>) -> Unit)
    fun close()
}
internal class HostedPlayBillingHost(private val hosted: HostedAccountClient) : PlayBillingHost {
    override suspend fun snapshot(accountId: String) = hosted.captureBillingSession(accountId)
    override suspend fun context(snapshot: BillingSessionSnapshot) = hosted.playBillingContext(snapshot)
    override suspend fun link(snapshot: BillingSessionSnapshot, operationId: String, productId: String, token: String) =
        hosted.linkPlayPurchase(snapshot, operationId, productId, token)
    override suspend fun current(snapshot: BillingSessionSnapshot) = hosted.isBillingSessionCurrent(snapshot)
}
internal class AndroidPlayBillingGateway(context: Context) : PlayBillingGateway {
    private var events: (Int, List<BillingPurchase>) -> Unit = { _, _ -> }
    private val details = mutableMapOf<String, com.android.billingclient.api.ProductDetails>()
    private val client = BillingClient.newBuilder(context.applicationContext).setListener(PurchasesUpdatedListener { result, purchases ->
        events(result.responseCode, purchases.orEmpty().map { it.toPort() })
    }).enablePendingPurchases(com.android.billingclient.api.PendingPurchasesParams.newBuilder().enableOneTimeProducts().build()).build()
    override fun setEvents(listener: (Int, List<BillingPurchase>) -> Unit) { events = listener }
    override fun connect(onReady: (Boolean) -> Unit) { if (client.isReady) onReady(true) else client.startConnection(object : BillingClientStateListener {
        override fun onBillingSetupFinished(result: com.android.billingclient.api.BillingResult) = onReady(result.responseCode == BillingClient.BillingResponseCode.OK)
        override fun onBillingServiceDisconnected() = Unit
    }) }
    override fun queryOffers(productId: String, onResult: (Boolean, List<BillingOffer>) -> Unit) = client.queryProductDetailsAsync(
        QueryProductDetailsParams.newBuilder().setProductList(listOf(QueryProductDetailsParams.Product.newBuilder().setProductId(productId).setProductType(BillingClient.ProductType.SUBS).build())).build(),
    ) { result, output ->
        val offers = output.productDetailsList.orEmpty().flatMap { detail -> detail.subscriptionOfferDetails.orEmpty().mapNotNull { offer ->
            if (offer.offerId != null) return@mapNotNull null
            val phase = offer.pricingPhases.pricingPhaseList.singleOrNull() ?: return@mapNotNull null
            val annual = when (phase.billingPeriod) { "P1M" -> false; "P1Y" -> true; else -> return@mapNotNull null }
            if (phase.recurrenceMode != com.android.billingclient.api.ProductDetails.RecurrenceMode.INFINITE_RECURRING) return@mapNotNull null
            val value = BillingOffer(detail.productId, offer.basePlanId, offer.offerToken, phase.formattedPrice, annual)
            details[key(value)] = detail; value
        } }
        onResult(result.responseCode == BillingClient.BillingResponseCode.OK, offers)
    }
    override fun launch(activity: Activity, offer: BillingOffer, accountBinding: String): Int {
        val detail = details[key(offer)] ?: return BillingClient.BillingResponseCode.ERROR
        val params = BillingFlowParams.ProductDetailsParams.newBuilder().setProductDetails(detail).setOfferToken(offer.offerToken).build()
        return client.launchBillingFlow(activity, BillingFlowParams.newBuilder().setProductDetailsParamsList(listOf(params)).setObfuscatedAccountId(accountBinding).build()).responseCode
    }
    override fun queryPurchases(onResult: (Boolean, List<BillingPurchase>) -> Unit) = client.queryPurchasesAsync(QueryPurchasesParams.newBuilder().setProductType(BillingClient.ProductType.SUBS).build()) { result, purchases -> onResult(result.responseCode == BillingClient.BillingResponseCode.OK, purchases.map { it.toPort() }) }
    override fun close() = client.endConnection()
    private fun key(offer: BillingOffer) = "${offer.productId}:${offer.basePlanId}:${offer.offerToken}"
    private fun Purchase.toPort() = BillingPurchase(purchaseToken, products, accountIdentifiers?.obfuscatedAccountId, purchaseState == Purchase.PurchaseState.PENDING)
}
