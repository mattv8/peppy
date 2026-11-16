package dev.peppy.mobile.billing

import android.app.Activity
import android.content.Context
import android.net.Uri
import com.android.billingclient.api.BillingClient
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.account.HostedAccountClient
import dev.peppy.mobile.account.HostedFailure
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.peppy_mobile_bindings.NativeBillingProvider
import uniffi.peppy_mobile_bindings.NativePlayBillingContext
import uniffi.peppy_mobile_bindings.NativePlayLinkOutcome

internal fun isPlayDistribution() = BuildConfig.PEPPY_DISTRIBUTION == "play"

internal fun isValidatedPlayProductId(productId: String?) =
    productId != null && productId.length <= 128 && Regex("[A-Za-z0-9._-]{1,128}").matches(productId)

internal fun playManageUri(
    context: NativePlayBillingContext,
    packageName: String = BuildConfig.APPLICATION_ID,
): Uri? = if (context.currentProvider != NativeBillingProvider.PLAY) {
    null
} else {
    context.productId
    ?.takeIf(::isValidatedPlayProductId)
    ?.let {
        Uri.Builder()
            .scheme("https")
            .authority("play.google.com")
            .appendPath("store")
            .appendPath("account")
            .appendPath("subscriptions")
            .appendQueryParameter("sku", it)
            .appendQueryParameter("package", packageName)
            .build()
    } ?: Uri.parse("https://play.google.com/store/account/subscriptions")
}

internal data class PlaySubscriptionPlan(
    val basePlanId: String,
    val offerToken: String,
    val price: String,
    val annual: Boolean,
)

internal enum class PlayBillingError { UNAVAILABLE, NETWORK, NOT_SUPPORTED, GENERIC }

internal sealed interface PlaySubscriptionState {
    data object Loading : PlaySubscriptionState
    data object SignInRequired : PlaySubscriptionState
    data object BillingUnavailable : PlaySubscriptionState
    data object Reconnecting : PlaySubscriptionState
    data object NotOffered : PlaySubscriptionState
    data object OtherProvider : PlaySubscriptionState
    data object UnknownProvider : PlaySubscriptionState
    data class VaultMismatch(val context: NativePlayBillingContext) : PlaySubscriptionState
    data class Ready(val context: NativePlayBillingContext, val plans: List<PlaySubscriptionPlan>) : PlaySubscriptionState
    data class ProviderPlay(val context: NativePlayBillingContext) : PlaySubscriptionState

    data object Purchasing : PlaySubscriptionState
    data object RestoreWorking : PlaySubscriptionState
    data object Pending : PlaySubscriptionState
    data object Verifying : PlaySubscriptionState
    data object RestoreNone : PlaySubscriptionState
    data object LinkRetry : PlaySubscriptionState
    data object NotEntitling : PlaySubscriptionState
    data object BoundToOtherAccount : PlaySubscriptionState
    data object WrongAccount : PlaySubscriptionState
    data class Error(val reason: PlayBillingError) : PlaySubscriptionState
}

internal enum class PlayBillingNotice { RESTORE_NONE }

internal data class PlayDeletionContext(val managed: Boolean, val productId: String?)

internal data class VerifiedAccountAccessAssociation(val origin: String, val vaultId: String) {
    fun matches(bindingKey: String): Boolean {
        val fields = bindingKey.split('\n')
        return fields.getOrNull(0) == origin && fields.getOrNull(1) == vaultId
    }
}

internal class PlayBillingCoordinator internal constructor(
    private val host: PlayBillingHost,
    private val gateway: PlayBillingGateway,
    private val scope: CoroutineScope,
    private val refreshAccount: suspend () -> Unit,
    private val waitForRecovery: suspend (Long) -> Unit = { delay(it) },
) {
    constructor(
        context: Context,
        hosted: HostedAccountClient,
        scope: CoroutineScope,
        refreshAccount: suspend () -> Unit,
    ) : this(HostedPlayBillingHost(hosted), AndroidPlayBillingGateway(context), scope, refreshAccount)

    private data class Identity(
        val generation: Long,
        val snapshot: BillingSessionSnapshot,
        val context: NativePlayBillingContext,
    )

    private data class MismatchKey(
        val accountId: String,
        val sessionFence: String,
        val accountVaultId: String?,
        val deviceVaultId: String,
    )

    private data class TokenKey(val sessionFence: String, val token: String)

    private val mutableState = MutableStateFlow<PlaySubscriptionState>(PlaySubscriptionState.Loading)
    val state: StateFlow<PlaySubscriptionState> = mutableState.asStateFlow()

    private val mutableNotice = MutableStateFlow<PlayBillingNotice?>(null)
    val notice: StateFlow<PlayBillingNotice?> = mutableNotice.asStateFlow()

    private val mutableLinked = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val linked: SharedFlow<Unit> = mutableLinked.asSharedFlow()
    private val mutableDeletionContext = MutableStateFlow(PlayDeletionContext(false, null))
    /** Retains only a server-verified Play provider across transient recovery UI states. */
    val deletionContext: StateFlow<PlayDeletionContext> = mutableDeletionContext.asStateFlow()
    private val mutableAccountAccessAssociation = MutableStateFlow<VerifiedAccountAccessAssociation?>(null)
    /** The authenticated billing snapshot's exact hosted origin and enrolled vault, if verified. */
    val accountAccessAssociation: StateFlow<VerifiedAccountAccessAssociation?> = mutableAccountAccessAssociation.asStateFlow()

    private var generation = 0L
    private var identity: Identity? = null
    private var requestedAccountId: String? = null
    private var enrolledVaultId: String? = null
    private var acknowledgedMismatch: MismatchKey? = null
    private var offers: List<BillingOffer> = emptyList()
    private var selectedOffer: BillingOffer? = null
    private var baseState: PlaySubscriptionState? = null
    private var launchIdentity: Identity? = null
    private var billingReadyGeneration: Long? = null
    private var restoreRequest = 0L
    private var operationRevision = 0L
    private var recoveryRevision: Long? = null
    private val processingTokens = mutableSetOf<TokenKey>()
    private val linkedTokens = mutableSetOf<TokenKey>()
    private var released = false

    private companion object {
        const val RECOVERY_ATTEMPTS = 3
        const val RECOVERY_BACKOFF_MS = 250L
    }

    init {
        gateway.setEvents(::onPurchasesUpdated)
        gateway.setDisconnectedListener {
            val current = identity ?: return@setDisconnectedListener
            if (!released && mutableState.value !in setOf(
                    baseState,
                    PlaySubscriptionState.Pending,
                    PlaySubscriptionState.Purchasing,
                )
            ) {
                recoverConnection(current)
            }
        }
    }

    fun load(accountId: String?, enrolledVaultId: String? = null) {
        requestedAccountId = accountId
        val loadGeneration = ++generation
        beginNewOperation()
        clearLoadedIdentity(enrolledVaultId)
        mutableNotice.value = null
        if (accountId == null) {
            mutableState.value = PlaySubscriptionState.SignInRequired
            return
        }
        mutableState.value = PlaySubscriptionState.Loading
        scope.launch {
            try {
                val captured = host.snapshot(accountId)
                if (!isGenerationCurrent(loadGeneration) || !host.current(captured)) return@launch
                val loadedContext = host.context(captured)
                if (!isGenerationCurrent(loadGeneration) || !host.current(captured)) return@launch
                if (loadedContext.accountId != captured.accountId) return@launch

                val loadedIdentity = Identity(loadGeneration, captured, loadedContext)
                identity = loadedIdentity
                mutableAccountAccessAssociation.value = captured.vaultId
                    ?.takeIf { it == enrolledVaultId }
                    ?.let { VerifiedAccountAccessAssociation(HostedAccountClient.ORIGIN, it) }
                routeLoadedIdentity(loadedIdentity)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: HostedFailure) {
                if (isGenerationCurrent(loadGeneration)) mutableState.value = stateFor(failure)
            } catch (_: Exception) {
                if (isGenerationCurrent(loadGeneration)) {
                    mutableState.value = PlaySubscriptionState.Error(PlayBillingError.NETWORK)
                }
            }
        }
    }

    fun select(plan: PlaySubscriptionPlan) {
        val current = identity ?: return
        if (!isIdentityCurrentLocally(current) || baseState !is PlaySubscriptionState.Ready) return
        selectedOffer = offers.firstOrNull {
            it.basePlanId == plan.basePlanId && it.offerToken == plan.offerToken
        }
    }

    fun continueWithSignedInAccount() {
        val current = identity ?: return
        val deviceVaultId = enrolledVaultId ?: return
        acknowledgedMismatch = mismatchKey(current.snapshot, deviceVaultId)
        load(current.snapshot.accountId, deviceVaultId)
    }

    fun purchase(activity: Activity) {
        val current = identity ?: return
        val offer = selectedOffer ?: return
        if (baseState !is PlaySubscriptionState.Ready) return
        beginNewOperation()
        scope.launch {
            if (!isIdentityCurrent(current) || current.context.currentProvider != NativeBillingProvider.NONE) return@launch
            val binding = current.context.accountBinding?.takeIf(String::isNotBlank) ?: return@launch
            restoreRequest++
            mutableState.value = PlaySubscriptionState.Purchasing
            launchIdentity = current
            val responseCode = gateway.launch(activity, offer, binding)
            when (responseCode) {
                BillingClient.BillingResponseCode.OK -> Unit
                BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED -> {
                    launchIdentity = null
                    restore(current, automatic = false)
                }
                else -> {
                    launchIdentity = null
                    if (responseCode in setOf(
                            BillingClient.BillingResponseCode.SERVICE_DISCONNECTED,
                            BillingClient.BillingResponseCode.SERVICE_UNAVAILABLE,
                        )
                    ) {
                        recoverConnection(current)
                    } else {
                        mutableState.value = billingStateFor(responseCode)
                    }
                }
            }
        }
    }

    fun restore() {
        identity?.let { restore(it, automatic = false) }
    }

    fun onResume() {
        identity?.let { restore(it, automatic = true) }
    }

    fun retry() {
        val accountId = requestedAccountId
        if (accountId == null) {
            mutableState.value = PlaySubscriptionState.SignInRequired
        } else {
            load(accountId, enrolledVaultId)
        }
    }

    private suspend fun routeLoadedIdentity(current: Identity) {
        val mismatch = enrolledVaultId?.let { mismatchKey(current.snapshot, it) }
        if (mismatch != null && current.snapshot.vaultId != mismatch.deviceVaultId && acknowledgedMismatch != mismatch) {
            setBaseState(PlaySubscriptionState.VaultMismatch(current.context))
            return
        }

        when (current.context.currentProvider) {
            NativeBillingProvider.PLAY -> setBaseState(PlaySubscriptionState.ProviderPlay(current.context))
            NativeBillingProvider.STRIPE -> setBaseState(PlaySubscriptionState.OtherProvider)
            NativeBillingProvider.UNKNOWN -> setBaseState(PlaySubscriptionState.UnknownProvider)
            NativeBillingProvider.NONE -> {
                val context = current.context
                if (!context.available || context.accountBinding.isNullOrBlank() || !isValidatedPlayProductId(context.productId)) {
                    setBaseState(PlaySubscriptionState.NotOffered)
                } else {
                    connectForOffers(current)
                }
            }
        }
    }

    private fun connectForOffers(current: Identity) {
        connect(current) {
            val productId = current.context.productId ?: return@connect
            gateway.queryOffers(productId) { success, returnedOffers ->
                runCallback(current) {
                    if (!success) {
                        setBaseState(PlaySubscriptionState.BillingUnavailable)
                        return@runCallback
                    }
                    val allowedOffers = allowedOffers(current, productId, returnedOffers)
                    if (allowedOffers.isEmpty()) {
                        setBaseState(PlaySubscriptionState.NotOffered)
                        return@runCallback
                    }
                    setReady(current, allowedOffers)
                    queryPurchases(current, automatic = true)
                }
            }
        }
    }

    private fun restore(current: Identity, automatic: Boolean) {
        if (!isRestoreAllowed(current)) return
        beginNewOperation()
        scope.launch {
            if (!isIdentityCurrent(current)) return@launch
            if (!automatic) {
                mutableNotice.value = null
                mutableState.value = PlaySubscriptionState.RestoreWorking
            }
            if (billingReadyGeneration == current.generation) {
                queryPurchases(current, automatic)
            } else {
                connect(current) { queryPurchases(current, automatic) }
            }
        }
    }

    private fun connect(current: Identity, onReady: () -> Unit) {
        gateway.connect { ready ->
            runCallback(current) {
                if (!ready) {
                    recoverConnection(current)
                    return@runCallback
                }
                billingReadyGeneration = current.generation
                onReady()
            }
        }
    }

    private fun queryPurchases(current: Identity, automatic: Boolean) {
        val request = ++restoreRequest
        gateway.queryPurchases { success, purchases ->
            runCallback(current) {
                if (request != restoreRequest) return@runCallback
                if (!success) {
                    if (!automatic) mutableState.value = PlaySubscriptionState.Error(PlayBillingError.UNAVAILABLE)
                    else restoreBaseState()
                    return@runCallback
                }
                handlePurchases(current, purchases, automatic)
            }
        }
    }

    private fun recoverConnection(current: Identity) {
        if (!isIdentityCurrentLocally(current)) return
        if (recoveryRevision != null) return
        val revision = ++operationRevision
        recoveryRevision = revision
        recoverConnection(current, revision, attempt = 1)
    }

    private fun recoverConnection(current: Identity, revision: Long, attempt: Int) {
        if (!isRecoveryCurrent(current, revision)) return
        mutableState.value = PlaySubscriptionState.Reconnecting
        scope.launch {
            if (attempt > 1) waitForRecovery(RECOVERY_BACKOFF_MS * (1L shl (attempt - 2)))
            if (!isRecoveryCurrent(current, revision)) return@launch
            gateway.connect { ready ->
                runCallback(current) {
                    if (!isRecoveryCurrent(current, revision)) return@runCallback
                    if (!ready) {
                        retryRecovery(current, revision, attempt)
                        return@runCallback
                    }
                    billingReadyGeneration = current.generation
                    val productId = current.context.productId
                    if (productId == null) {
                        retryRecovery(current, revision, attempt)
                        return@runCallback
                    }
                    gateway.queryOffers(productId) { offersAvailable, returnedOffers ->
                        runCallback(current) {
                            if (!isRecoveryCurrent(current, revision)) return@runCallback
                            if (!offersAvailable) {
                                retryRecovery(current, revision, attempt)
                                return@runCallback
                            }
                            if (current.context.currentProvider == NativeBillingProvider.NONE) {
                                val allowedOffers = allowedOffers(current, productId, returnedOffers)
                                if (allowedOffers.isEmpty()) {
                                    retryRecovery(current, revision, attempt)
                                    return@runCallback
                                }
                                setReady(current, allowedOffers)
                            } else {
                                setBaseState(PlaySubscriptionState.ProviderPlay(current.context))
                                mutableDeletionContext.value = PlayDeletionContext(true, current.context.productId)
                            }
                            recoveryRevision = null
                            queryPurchases(current, automatic = true)
                        }
                    }
                }
            }
        }
    }

    private fun retryRecovery(current: Identity, revision: Long, attempt: Int) {
        if (!isRecoveryCurrent(current, revision)) return
        if (attempt >= RECOVERY_ATTEMPTS) {
            recoveryRevision = null
            mutableState.value = PlaySubscriptionState.Error(PlayBillingError.UNAVAILABLE)
        } else {
            recoverConnection(current, revision, attempt + 1)
        }
    }

    private suspend fun handlePurchases(
        current: Identity,
        purchases: List<BillingPurchase>,
        automatic: Boolean,
    ) {
        val validPurchases = purchases.filter { it.matches(current.context) }
        val pendingPurchases = validPurchases.filter(BillingPurchase::pending)
        val purchased = validPurchases.filterNot(BillingPurchase::pending)

        if (pendingPurchases.isNotEmpty()) mutableState.value = PlaySubscriptionState.Pending
        purchased.forEach { link(it, current) }
        if (purchased.isEmpty() && pendingPurchases.isEmpty()) {
            restoreBaseState()
            if (!automatic) mutableNotice.value = PlayBillingNotice.RESTORE_NONE
        }
    }

    private fun link(purchase: BillingPurchase, current: Identity) {
        val key = TokenKey(current.snapshot.fence, purchase.token)
        if (key in linkedTokens || !processingTokens.add(key)) return
        mutableState.value = PlaySubscriptionState.Verifying
        scope.launch {
            try {
                if (!isIdentityCurrent(current)) return@launch
                val productId = current.context.productId ?: return@launch
                val outcome = host.link(
                    current.snapshot,
                    UUID.randomUUID().toString(),
                    productId,
                    purchase.token,
                )
                if (!isIdentityCurrent(current)) return@launch
                when (outcome) {
                    NativePlayLinkOutcome.LINKED -> {
                        beginNewOperation()
                        linkedTokens += key
                        baseState = null
                        val verifiedContext = current.context.copy(currentProvider = NativeBillingProvider.PLAY)
                        identity = current.copy(context = verifiedContext)
                        setBaseState(PlaySubscriptionState.ProviderPlay(verifiedContext))
                        mutableDeletionContext.value = PlayDeletionContext(true, verifiedContext.productId)
                        mutableLinked.emit(Unit)
                        if (isIdentityCurrent(current)) refreshAccount()
                    }
                    NativePlayLinkOutcome.PENDING -> mutableState.value = PlaySubscriptionState.Pending
                    NativePlayLinkOutcome.BOUND_TO_OTHER_ACCOUNT -> {
                        mutableState.value = PlaySubscriptionState.BoundToOtherAccount
                    }
                    NativePlayLinkOutcome.NOT_ENTITLING -> mutableState.value = PlaySubscriptionState.NotEntitling
                    else -> mutableState.value = PlaySubscriptionState.LinkRetry
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: HostedFailure) {
                if (isIdentityCurrent(current)) {
                    mutableState.value = if (failure.kind == HostedFailure.Kind.SESSION_EXPIRED) {
                        PlaySubscriptionState.SignInRequired
                    } else {
                        PlaySubscriptionState.LinkRetry
                    }
                }
            } catch (_: Exception) {
                if (isIdentityCurrent(current)) mutableState.value = PlaySubscriptionState.LinkRetry
            } finally {
                processingTokens -= key
            }
        }
    }

    private fun onPurchasesUpdated(code: Int, purchases: List<BillingPurchase>) {
        val launched = launchIdentity ?: return
        runCallback(launched) {
            launchIdentity = null
            when (code) {
                BillingClient.BillingResponseCode.OK -> {
                    val valid = purchases.filter { it.matches(launched.context) }
                    if (valid.isEmpty()) {
                        restoreBaseState()
                    } else {
                        handlePurchases(launched, valid, automatic = true)
                    }
                }
                BillingClient.BillingResponseCode.USER_CANCELED -> restoreBaseState()
                BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED -> restore(launched, automatic = false)
                BillingClient.BillingResponseCode.SERVICE_DISCONNECTED,
                BillingClient.BillingResponseCode.SERVICE_UNAVAILABLE -> recoverConnection(launched)
                else -> mutableState.value = billingStateFor(code)
            }
        }
    }

    private fun runCallback(current: Identity, action: suspend () -> Unit) {
        scope.launch {
            if (isIdentityCurrent(current)) action()
        }
    }

    private suspend fun isIdentityCurrent(current: Identity): Boolean =
        isIdentityCurrentLocally(current) && host.current(current.snapshot)

    private fun isIdentityCurrentLocally(current: Identity): Boolean {
        val loaded = identity ?: return false
        return !released &&
            current.generation == generation &&
            loaded.generation == generation &&
            loaded.snapshot == current.snapshot
    }

    private fun isGenerationCurrent(candidate: Long): Boolean = !released && candidate == generation

    private fun isRestoreAllowed(current: Identity): Boolean {
        if (!isIdentityCurrentLocally(current)) return false
        return when (current.context.currentProvider) {
            NativeBillingProvider.NONE -> baseState is PlaySubscriptionState.Ready
            NativeBillingProvider.PLAY -> baseState is PlaySubscriptionState.ProviderPlay
            else -> false
        }
    }

    private fun setBaseState(value: PlaySubscriptionState) {
        baseState = value
        mutableState.value = value
        if (value is PlaySubscriptionState.ProviderPlay) {
            mutableDeletionContext.value = PlayDeletionContext(true, value.context.productId)
        }
    }

    private fun restoreBaseState() {
        baseState?.let { mutableState.value = it }
    }

    private fun clearLoadedIdentity(newVaultId: String?) {
        identity = null
        enrolledVaultId = newVaultId
        offers = emptyList()
        selectedOffer = null
        baseState = null
        launchIdentity = null
        billingReadyGeneration = null
        restoreRequest++
        mutableDeletionContext.value = PlayDeletionContext(false, null)
        mutableAccountAccessAssociation.value = null
    }

    private fun allowedOffers(current: Identity, productId: String, returnedOffers: List<BillingOffer>) =
        returnedOffers
            .filter { it.productId == productId && it.basePlanId in current.context.basePlanIds }
            .distinctBy { it.basePlanId to it.offerToken }

    private fun setReady(current: Identity, allowedOffers: List<BillingOffer>) {
        offers = allowedOffers
        selectedOffer = allowedOffers.firstOrNull { !it.annual } ?: allowedOffers.first()
        setBaseState(PlaySubscriptionState.Ready(current.context, allowedOffers.map(BillingOffer::toPlan)))
    }

    private fun beginNewOperation() {
        operationRevision++
        recoveryRevision = null
    }

    private fun isRecoveryCurrent(current: Identity, revision: Long) =
        recoveryRevision == revision && operationRevision == revision && isIdentityCurrentLocally(current)

    private fun mismatchKey(snapshot: BillingSessionSnapshot, deviceVaultId: String) = MismatchKey(
        snapshot.accountId,
        snapshot.fence,
        snapshot.vaultId,
        deviceVaultId,
    )

    private fun stateFor(failure: HostedFailure): PlaySubscriptionState = when (failure.kind) {
        HostedFailure.Kind.SESSION_EXPIRED -> PlaySubscriptionState.SignInRequired
        HostedFailure.Kind.WRONG_ACCOUNT -> PlaySubscriptionState.WrongAccount
        else -> PlaySubscriptionState.Error(PlayBillingError.NETWORK)
    }

    fun release() {
        if (released) return
        released = true
        generation++
        clearLoadedIdentity(null)
        processingTokens.clear()
        gateway.close()
    }

}

private fun BillingOffer.toPlan() = PlaySubscriptionPlan(basePlanId, offerToken, price, annual)

private fun BillingPurchase.matches(context: NativePlayBillingContext): Boolean =
    products.singleOrNull() == context.productId &&
        !obfuscatedAccountId.isNullOrBlank() &&
        obfuscatedAccountId == context.accountBinding

internal fun billingStateFor(responseCode: Int): PlaySubscriptionState = when (responseCode) {
    BillingClient.BillingResponseCode.SERVICE_DISCONNECTED -> PlaySubscriptionState.Reconnecting
    BillingClient.BillingResponseCode.SERVICE_UNAVAILABLE,
    BillingClient.BillingResponseCode.BILLING_UNAVAILABLE -> PlaySubscriptionState.Error(PlayBillingError.UNAVAILABLE)
    BillingClient.BillingResponseCode.NETWORK_ERROR -> PlaySubscriptionState.Error(PlayBillingError.NETWORK)
    BillingClient.BillingResponseCode.FEATURE_NOT_SUPPORTED -> PlaySubscriptionState.Error(PlayBillingError.NOT_SUPPORTED)
    else -> PlaySubscriptionState.Error(PlayBillingError.GENERIC)
}
