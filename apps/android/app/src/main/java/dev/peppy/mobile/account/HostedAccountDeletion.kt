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
import dev.peppy.mobile.ui.ConsentSheet

internal const val HOSTED_ACCOUNT_DELETION_URL = "https://peppy.pro/account/delete"
internal const val PLAY_SUBSCRIPTIONS_URL = "https://play.google.com/store/account/subscriptions"

private fun openExternalUrl(context: Context, url: String): Boolean = try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    true
} catch (_: ActivityNotFoundException) {
    false
}

internal fun openHostedAccountDeletion(context: Context): Boolean = openExternalUrl(context, HOSTED_ACCOUNT_DELETION_URL)

internal fun openPlaySubscriptionList(context: Context): Boolean = openExternalUrl(context, PLAY_SUBSCRIPTIONS_URL)

/** The external account-deletion flow is deliberately separate from local vault deletion. */
@Composable
internal fun HostedAccountDeletionEntry(
    tag: String,
    includeVaultDistinction: Boolean = false,
    onOpenFailed: () -> Unit,
    onViewSubscription: (() -> Unit)? = null,
    isPlay: Boolean = BuildConfig.PEPPY_DISTRIBUTION == "play",
    openDeletion: (Context) -> Boolean = ::openHostedAccountDeletion,
    openPlaySubscriptions: (Context) -> Boolean = ::openPlaySubscriptionList,
) {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    TextButton(onClick = { open = true }, modifier = Modifier.testTag(tag)) {
        Text(stringResource(R.string.peppy_account_delete_row))
    }
    if (includeVaultDistinction) {
        Text(stringResource(R.string.peppy_account_delete_vault_distinct))
    }
    val viewSubscription: () -> Unit = {
        open = false
        if (onViewSubscription != null) onViewSubscription() else if (!openPlaySubscriptions(context)) onOpenFailed()
    }
    if (open) ConsentSheet(
        title = stringResource(R.string.peppy_account_delete_title),
        body = stringResource(R.string.peppy_account_delete_body),
        nextStep = if (isPlay) {
            stringResource(R.string.peppy_account_delete_subscription)
        } else {
            null
        },
        tag = "account-delete-sheet",
        onDismiss = { open = false },
        onConfirm = {
            open = false
            if (!openDeletion(context)) onOpenFailed()
        },
        confirmLabel = stringResource(R.string.peppy_account_delete_continue),
        secondaryActionLabel = if (isPlay) {
            stringResource(R.string.peppy_account_delete_view_subscription)
        } else {
            null
        },
        onSecondaryAction = if (isPlay) viewSubscription else null,
    )
}
