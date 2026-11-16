package dev.peppy.mobile.mms

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.googlemessages.GoogleMessagesLink
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
internal fun RestrictedNotAllowedAccessControls(
    context: Context,
    refresh: Int,
    onOpenGoogleMessagesLink: () -> Unit,
) {
    val available = GoogleMessagesLink.available(context)
    var linked by remember(context, available) { mutableStateOf<Boolean?>(null) }
    LaunchedEffect(context, available, refresh) {
        linked = if (available) withContext(Dispatchers.IO) { GoogleMessagesLink.linked(context) } else false
    }
    RestrictedMessageAccessStatus(
        RestrictedMessageAccess.NOT_ALLOWED,
        if (linked == true) R.string.peppy_mms_restricted_play_status_linked else null,
    )
    RestrictedAccessAction(linked, available, onOpenGoogleMessagesLink)
}

@Composable
internal fun RestrictedAccessAction(
    linked: Boolean?,
    available: Boolean,
    onOpenGoogleMessagesLink: () -> Unit,
) {
    when {
        !available -> Text(
            stringResource(R.string.peppy_mms_restricted_play_unavailable),
            Modifier.testTag("mms-restricted-link-unavailable"),
        )
        linked == true -> RestrictedAccessLinkAction(
            body = R.string.peppy_mms_restricted_play_linked_body,
            bodyTag = "mms-restricted-link-linked",
            action = R.string.peppy_google_messages_link_action_manage,
            actionTag = "mms-restricted-link-manage",
            onOpenGoogleMessagesLink = onOpenGoogleMessagesLink,
        )
        linked == false -> RestrictedAccessLinkAction(
            body = R.string.peppy_mms_restricted_play_link_body,
            bodyTag = "mms-restricted-link-body",
            action = R.string.peppy_google_messages_link_action_link,
            actionTag = "mms-restricted-link-action",
            onOpenGoogleMessagesLink = onOpenGoogleMessagesLink,
        )
        else -> Unit
    }
}

@Composable
private fun RestrictedAccessLinkAction(
    body: Int,
    bodyTag: String,
    action: Int,
    actionTag: String,
    onOpenGoogleMessagesLink: () -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().testTag("mms-restricted-link-action-container"),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(stringResource(body), Modifier.testTag(bodyTag))
        OutlinedButton(
            onClick = onOpenGoogleMessagesLink,
            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(actionTag),
        ) { Text(stringResource(action)) }
    }
}

internal fun restrictedMessageAccessHelpResId(): Int = R.string.peppy_mms_restricted_play_help
