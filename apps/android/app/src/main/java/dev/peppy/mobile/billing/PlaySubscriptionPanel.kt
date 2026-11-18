package dev.peppy.mobile.billing

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.account.AccountAccessState
import dev.peppy.mobile.account.CachedAccountNotice
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.NativeAccountNoticeKind

/** Shared Play-only subscription surface. Direct builds must not compose this component. */
@Composable
internal fun PlaySubscriptionPanel(
    coordinator: PlayBillingCoordinator,
    accountId: String?,
    enrolledVaultId: String? = null,
    loadRevision: Int = 0,
    onSignIn: () -> Unit,
    signInError: String? = null,
    showRestoreActions: Boolean = true,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val state by coordinator.state.collectAsState()
    val notice by coordinator.notice.collectAsState()
    val accountAccessAssociation by coordinator.accountAccessAssociation.collectAsState()
    val access by AccountAccessState.access.collectAsState()
    var selected by remember { mutableStateOf<PlaySubscriptionPlan?>(null) }
    LaunchedEffect(Unit) { withContext(Dispatchers.IO) { AccountAccessState.ensureLoaded(context) } }
    PlaySubscriptionLoadEffect(accountId, enrolledVaultId, loadRevision, coordinator::load)
    PlaySubscriptionFeedbackLayout(signInError, modifier) {
        if (showRestoreActions && notice == PlayBillingNotice.RESTORE_NONE) Text(
            stringResource(R.string.peppy_play_restore_none),
            Modifier.testTag("play-restore-none-notice").semantics { liveRegion = LiveRegionMode.Polite },
        )
        when (val current = state) {
            PlaySubscriptionState.Loading -> Text(stringResource(R.string.peppy_play_checking), Modifier.testTag("play-subscription-loading"))
            PlaySubscriptionState.SignInRequired -> {
                Text(stringResource(R.string.peppy_play_signin_required_body), Modifier.testTag("play-subscription-status"))
                Button(onClick = onSignIn, modifier = Modifier.fillMaxWidth().testTag("play-subscription-signin")) {
                    Text(stringResource(R.string.peppy_hosted_sign_in_google))
                }
            }
            PlaySubscriptionState.BillingUnavailable -> RecoverableStatus(R.string.peppy_play_billing_unavailable_body, coordinator::retry)
            PlaySubscriptionState.Reconnecting -> Status(R.string.peppy_play_reconnecting, "play-subscription-reconnecting")
            PlaySubscriptionState.NotOffered -> RecoverableStatus(R.string.peppy_play_not_offered_body, coordinator::retry)
            PlaySubscriptionState.OtherProvider -> Status(R.string.peppy_play_other_provider_body)
            PlaySubscriptionState.UnknownProvider -> RecoverableStatus(R.string.peppy_play_unknown_provider_body, coordinator::retry)
            is PlaySubscriptionState.VaultMismatch -> {
                Text(stringResource(R.string.peppy_play_vault_mismatch_title), Modifier.testTag("play-subscription-status"))
                Text(stringResource(R.string.peppy_play_vault_mismatch_body))
                Button(onClick = coordinator::continueWithSignedInAccount, modifier = Modifier.testTag("play-vault-mismatch-continue")) {
                    Text(stringResource(R.string.peppy_play_vault_mismatch_continue))
                }
                OutlinedButton(onClick = onSignIn, modifier = Modifier.testTag("play-subscription-switch-account")) {
                    Text(stringResource(R.string.peppy_play_switch_account))
                }
            }
            is PlaySubscriptionState.ProviderPlay -> ProviderPlay(
                current.context,
                access?.takeIf { cached ->
                    accountAccessAssociation?.matches(cached.bindingKey) == true
                }?.notice,
                context,
                coordinator::restore,
                showRestoreActions,
            )
            is PlaySubscriptionState.Ready -> {
                val chosen = selected?.takeIf { it in current.plans }
                    ?: current.plans.firstOrNull { !it.annual } ?: current.plans.first()
                if (current.plans.size > 1) {
                    Text(stringResource(R.string.peppy_play_plan_group_label), style = MaterialTheme.typography.titleSmall, modifier = Modifier.testTag("play-subscription-plans"))
                    current.plans.forEach { plan -> PlanRow(plan, plan == chosen) { selected = plan; coordinator.select(plan) } }
                } else PlanRow(chosen, true) { coordinator.select(chosen) }
                Text(
                    stringResource(if (chosen.annual) R.string.peppy_play_summary_annual else R.string.peppy_play_summary_monthly, chosen.price),
                    modifier = Modifier.testTag("play-subscription-summary"),
                )
                Column(Modifier.testTag("play-subscription-terms")) {
                    Text(stringResource(if (chosen.annual) R.string.peppy_play_terms_annual else R.string.peppy_play_terms_monthly), style = MaterialTheme.typography.bodySmall)
                    Text(stringResource(R.string.peppy_play_terms_renewal), style = MaterialTheme.typography.bodySmall)
                    Text(stringResource(R.string.peppy_play_terms_cancel), style = MaterialTheme.typography.bodySmall)
                    Text(stringResource(R.string.peppy_play_terms_account), style = MaterialTheme.typography.bodySmall)
                }
                Button(
                    onClick = { (context as? Activity)?.let { coordinator.select(chosen); coordinator.purchase(it) } },
                    modifier = Modifier.fillMaxWidth().testTag("play-subscribe-button"),
                ) { Text(stringResource(R.string.peppy_hosted_subscribe_cta)) }
                if (showRestoreActions) {
                    OutlinedButton(onClick = coordinator::restore, modifier = Modifier.fillMaxWidth().testTag("play-restore-button")) {
                        Text(stringResource(R.string.peppy_play_restore_cta))
                    }
                }
            }
            PlaySubscriptionState.Purchasing -> Text(stringResource(R.string.peppy_play_checking), Modifier.testTag("play-subscription-status"))
            PlaySubscriptionState.RestoreWorking -> Text(stringResource(R.string.peppy_play_restore_working), Modifier.testTag("play-subscription-status"))
            PlaySubscriptionState.Pending -> {
                Text(stringResource(R.string.peppy_play_pending_title), Modifier.testTag("play-subscription-status"))
                Text(stringResource(R.string.peppy_play_pending_body))
                if (showRestoreActions) {
                    OutlinedButton(onClick = coordinator::restore, modifier = Modifier.testTag("play-pending-check")) { Text(stringResource(R.string.peppy_play_pending_check)) }
                }
            }
            PlaySubscriptionState.Verifying -> Text(
                stringResource(R.string.peppy_hosted_purchase_verifying),
                Modifier.testTag("play-subscription-status").semantics { liveRegion = LiveRegionMode.Polite },
            )
            PlaySubscriptionState.RestoreNone -> Status(R.string.peppy_play_restore_none)
            PlaySubscriptionState.LinkRetry -> {
                Status(R.string.peppy_play_link_retry_body)
                OutlinedButton(onClick = coordinator::retry, modifier = Modifier.testTag("play-link-retry")) { Text(stringResource(R.string.peppy_try_again)) }
            }
            PlaySubscriptionState.NotEntitling -> {
                Status(if (showRestoreActions) R.string.peppy_play_not_active_body else R.string.peppy_play_not_active_body_check)
                if (showRestoreActions) {
                    OutlinedButton(onClick = coordinator::restore, modifier = Modifier.testTag("play-restore-button")) { Text(stringResource(R.string.peppy_play_restore_cta)) }
                }
            }
            PlaySubscriptionState.BoundToOtherAccount -> {
                Text(stringResource(R.string.peppy_play_other_account_title), Modifier.testTag("play-subscription-error"))
                Text(stringResource(R.string.peppy_play_other_account_body))
                OutlinedButton(onClick = onSignIn, modifier = Modifier.testTag("play-subscription-switch-account")) { Text(stringResource(R.string.peppy_play_switch_account)) }
            }
            PlaySubscriptionState.WrongAccount -> {
                Status(R.string.peppy_play_wrong_account_body)
                OutlinedButton(onClick = onSignIn, modifier = Modifier.testTag("play-subscription-switch-account")) { Text(stringResource(R.string.peppy_play_switch_account)) }
            }
            is PlaySubscriptionState.Error -> {
                Text(
                    stringResource(
                        when (current.reason) {
                            PlayBillingError.UNAVAILABLE -> R.string.peppy_play_billing_unavailable_body
                            PlayBillingError.NETWORK -> R.string.peppy_play_error_network
                            PlayBillingError.NOT_SUPPORTED -> R.string.peppy_play_error_not_supported
                            PlayBillingError.GENERIC -> R.string.peppy_play_error_generic
                        },
                    ),
                    Modifier.testTag("play-subscription-error").semantics { liveRegion = LiveRegionMode.Polite },
                )
                OutlinedButton(onClick = coordinator::retry, modifier = Modifier.testTag("play-billing-retry")) { Text(stringResource(R.string.peppy_try_again)) }
            }
        }
    }
}

@Composable
internal fun PlaySubscriptionFeedbackLayout(
    signInError: String?,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    Column(
        modifier.testTag("play-subscription-panel"),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        signInError?.let { Text(it, Modifier.testTag("play-signin-error")) }
        content()
    }
}

@Composable
internal fun PlaySubscriptionLoadEffect(
    accountId: String?,
    enrolledVaultId: String?,
    loadRevision: Int,
    onLoad: (String?, String?) -> Unit,
) {
    LaunchedEffect(accountId, enrolledVaultId, loadRevision) { onLoad(accountId, enrolledVaultId) }
}

@Composable private fun Status(copy: Int, tag: String = "play-subscription-status") = Text(
    stringResource(copy),
    Modifier.testTag(tag).semantics { liveRegion = LiveRegionMode.Polite },
)

@Composable private fun RecoverableStatus(copy: Int, onRetry: () -> Unit) {
    Status(copy)
    OutlinedButton(onClick = onRetry, modifier = Modifier.testTag("play-billing-retry")) {
        Text(stringResource(R.string.peppy_try_again))
    }
}

@Composable private fun PlanRow(plan: PlaySubscriptionPlan, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).testTag("play-plan-option-${plan.basePlanId}")
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick),
    ) {
        RadioButton(selected, onClick = null)
        Column {
            Text(stringResource(if (plan.annual) R.string.peppy_play_plan_annual else R.string.peppy_play_plan_monthly))
            Text(stringResource(if (plan.annual) R.string.peppy_play_price_per_year else R.string.peppy_play_price_per_month, plan.price))
            Text(stringResource(if (plan.annual) R.string.peppy_play_billed_yearly else R.string.peppy_play_billed_monthly))
        }
    }
}

@Composable internal fun ProviderPlay(
    context: uniffi.peppy_mobile_bindings.NativePlayBillingContext,
    notice: CachedAccountNotice?,
    androidContext: android.content.Context,
    onRestore: () -> Unit,
    showRestoreActions: Boolean,
) {
    var manageFailure by remember { mutableStateOf(false) }
    Text(stringResource(R.string.peppy_play_provider_label), Modifier.testTag("play-subscription-provider"))
    Text(stringResource(R.string.peppy_play_provider_google_play))
    val openManage = {
        try {
            androidContext.startActivity(
                Intent(Intent.ACTION_VIEW, playManageUri(context, androidContext.packageName)
                    ?: Uri.parse("https://play.google.com/store/account/subscriptions")),
            )
        }
        catch (_: ActivityNotFoundException) { manageFailure = true }
    }
    val showsAccessAction = notice?.kind in setOf(
        NativeAccountNoticeKind.PAYMENT_FAILED_GRACE,
        NativeAccountNoticeKind.ENDING_SCHEDULED,
        NativeAccountNoticeKind.READ_ONLY_PAST_DUE,
    )
    val manageDescription = stringResource(R.string.peppy_play_manage_cta_description)
    PlayAccessNotice(notice, manageDescription, openManage)
    if (!showsAccessAction) {
        TextButton(
            onClick = openManage,
            modifier = Modifier.testTag("play-manage-button")
                .semantics { contentDescription = manageDescription },
        ) { Text(stringResource(R.string.peppy_play_manage_cta)) }
    }
    if (manageFailure) Text(stringResource(R.string.peppy_toggle_android_link_unavailable), Modifier.testTag("play-manage-error"))
    if (showRestoreActions) {
        OutlinedButton(onClick = onRestore, modifier = Modifier.testTag("play-restore-button")) {
            Text(stringResource(R.string.peppy_play_restore_cta))
        }
    }
}

@Composable
private fun PlayAccessNotice(notice: CachedAccountNotice?, manageDescription: String, onManage: () -> Unit) {
    val body = when (notice?.kind) {
        NativeAccountNoticeKind.PAYMENT_FAILED_GRACE -> R.string.peppy_play_grace_body
        NativeAccountNoticeKind.ENDING_SCHEDULED -> R.string.peppy_play_ending_body
        NativeAccountNoticeKind.READ_ONLY_PAST_DUE -> R.string.peppy_play_hold_body
        NativeAccountNoticeKind.READ_ONLY_CANCELED -> R.string.peppy_play_subscription_ended_body
        else -> return
    }
    val title = when (notice.kind) {
        NativeAccountNoticeKind.PAYMENT_FAILED_GRACE -> R.string.peppy_account_payment_failed_grace_title
        NativeAccountNoticeKind.READ_ONLY_PAST_DUE -> R.string.peppy_play_hold_title
        NativeAccountNoticeKind.ENDING_SCHEDULED -> R.string.peppy_account_ending_scheduled_title
        else -> R.string.peppy_play_subscription_ended_title
    }
    Column(
        Modifier.testTag("play-access-notice").semantics { liveRegion = LiveRegionMode.Polite },
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (notice.kind == NativeAccountNoticeKind.ENDING_SCHEDULED) {
            Text(
                stringResource(if (notice.deadline == null) R.string.peppy_play_ending_scheduled_nodate_title else title, formatNoticeDate(notice.deadline)),
                Modifier.semantics { heading() },
            )
        } else {
            Text(stringResource(title), Modifier.semantics { heading() })
        }
        if (notice.kind == NativeAccountNoticeKind.PAYMENT_FAILED_GRACE && notice.deadline != null) {
            Text(stringResource(body, formatNoticeDate(notice.deadline)))
        } else {
            Text(stringResource(if (notice.kind == NativeAccountNoticeKind.PAYMENT_FAILED_GRACE) R.string.peppy_play_grace_body_nodate else body))
        }
        if (notice.kind in setOf(
                NativeAccountNoticeKind.PAYMENT_FAILED_GRACE,
                NativeAccountNoticeKind.ENDING_SCHEDULED,
                NativeAccountNoticeKind.READ_ONLY_PAST_DUE,
            )
        ) {
            TextButton(
                onClick = onManage,
                modifier = Modifier.testTag("play-access-action").semantics {
                    contentDescription = manageDescription
                },
            ) {
                Text(stringResource(
                    if (notice.kind == NativeAccountNoticeKind.ENDING_SCHEDULED) {
                        R.string.peppy_play_resubscribe_cta
                    } else {
                        R.string.peppy_play_fix_payment_cta
                    },
                ))
            }
        }
    }
}

private fun formatNoticeDate(deadline: Long?): String =
    deadline?.let { DateFormat.getDateInstance().format(Date(it * 1_000)) }.orEmpty()
