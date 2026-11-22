package dev.peppy.mobile.account

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.R
import dev.peppy.mobile.billing.playManageUri
import uniffi.peppy_mobile_bindings.NativeBillingProvider
import uniffi.peppy_mobile_bindings.NativePlayBillingContext
import dev.peppy.mobile.ui.ConsentSheet
import dev.peppy.mobile.ui.DestructiveRow

internal const val HOSTED_ACCOUNT_URL = "https://peppy.pro/account"
internal const val HOSTED_ACCOUNT_DELETION_URL = "https://peppy.pro/account/delete"
internal const val PLAY_SUBSCRIPTIONS_URL = "https://play.google.com/store/account/subscriptions"

private fun openExternalUrl(context: Context, url: String): Boolean = try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    true
} catch (_: ActivityNotFoundException) {
    false
}

internal fun openHostedAccount(context: Context): Boolean = openExternalUrl(context, HOSTED_ACCOUNT_URL)

internal fun openHostedAccountDeletion(context: Context): Boolean = openExternalUrl(context, HOSTED_ACCOUNT_DELETION_URL)

internal fun openPlaySubscriptionList(context: Context): Boolean = openExternalUrl(context, PLAY_SUBSCRIPTIONS_URL)

internal fun openPlaySubscription(context: Context, productId: String?): Boolean =
    openExternalUrl(
        context,
        playManageUri(
            NativePlayBillingContext("", false, productId, emptyList(), null, NativeBillingProvider.PLAY),
            context.packageName,
        ).toString(),
    )

/** The external account-deletion flow is deliberately separate from local vault deletion. */
@Composable
internal fun HostedAccountDeletionEntry(
    tag: String,
    onOpenFailed: () -> Unit,
    onViewSubscription: (() -> Unit)? = null,
    isPlay: Boolean = BuildConfig.PEPPY_DISTRIBUTION == "play",
    playManaged: Boolean = false,
    playProductId: String? = null,
    openDeletion: (Context) -> Boolean = ::openHostedAccountDeletion,
    openPlaySubscriptions: (Context) -> Boolean = ::openPlaySubscriptionList,
) {
    var open by remember { mutableStateOf(false) }
    TextButton(onClick = { open = true }, modifier = Modifier.testTag(tag)) {
        Text(stringResource(R.string.peppy_account_delete_row))
    }
    if (open) HostedAccountDeletionSheet(
        onDismiss = { open = false },
        onOpenFailed = onOpenFailed,
        onViewSubscription = onViewSubscription,
        isPlay = isPlay,
        playManaged = playManaged,
        playProductId = playProductId,
        openDeletion = openDeletion,
        openPlaySubscriptions = openPlaySubscriptions,
    )
}

@Composable
internal fun HostedAccountDeletionRow(
    tag: String,
    onOpenFailed: () -> Unit,
    onViewSubscription: (() -> Unit)? = null,
    isPlay: Boolean = BuildConfig.PEPPY_DISTRIBUTION == "play",
    playManaged: Boolean = false,
    playProductId: String? = null,
    openDeletion: (Context) -> Boolean = ::openHostedAccountDeletion,
    openPlaySubscriptions: (Context) -> Boolean = ::openPlaySubscriptionList,
) {
    var open by remember { mutableStateOf(false) }
    DestructiveRow(
        tag = tag,
        label = stringResource(R.string.peppy_account_delete_row),
        consequence = stringResource(R.string.peppy_settings_delete_account_consequence),
        actionLabel = stringResource(R.string.peppy_settings_delete_action),
        onAction = { open = true },
    )
    if (open) HostedAccountDeletionSheet(
        onDismiss = { open = false },
        onOpenFailed = onOpenFailed,
        onViewSubscription = onViewSubscription,
        isPlay = isPlay,
        playManaged = playManaged,
        playProductId = playProductId,
        openDeletion = openDeletion,
        openPlaySubscriptions = openPlaySubscriptions,
    )
}

@Composable
private fun HostedAccountDeletionSheet(
    onDismiss: () -> Unit,
    onOpenFailed: () -> Unit,
    onViewSubscription: (() -> Unit)?,
    isPlay: Boolean,
    playManaged: Boolean,
    playProductId: String?,
    openDeletion: (Context) -> Boolean,
    openPlaySubscriptions: (Context) -> Boolean,
) {
    val context = LocalContext.current
    val viewSubscription: () -> Unit = {
        onDismiss()
        if (playManaged) {
            val opened = playProductId?.let { openPlaySubscription(context, it) } ?: openPlaySubscriptions(context)
            if (!opened) onOpenFailed()
        } else if (onViewSubscription != null) {
            onViewSubscription()
        } else if (!openPlaySubscriptions(context)) {
            onOpenFailed()
        }
    }
    ConsentSheet(
        title = stringResource(R.string.peppy_account_delete_title),
        body = stringResource(R.string.peppy_account_delete_body),
        nextStep = if (playManaged) {
            stringResource(R.string.peppy_account_delete_subscription_play)
        } else if (isPlay) {
            stringResource(R.string.peppy_account_delete_subscription)
        } else {
            null
        },
        tag = "account-delete-sheet",
        onDismiss = onDismiss,
        onConfirm = {
            onDismiss()
            if (!openDeletion(context)) onOpenFailed()
        },
        confirmLabel = stringResource(R.string.peppy_account_delete_continue),
        secondaryActionLabel = if (playManaged) {
            stringResource(R.string.peppy_account_delete_cancel_in_play)
        } else if (isPlay) {
            stringResource(R.string.peppy_account_delete_view_subscription)
        } else {
            null
        },
        onSecondaryAction = if (playManaged || isPlay) viewSubscription else null,
        secondaryActionTag = if (playManaged) "account-delete-play-cancel" else null,
    )
}
