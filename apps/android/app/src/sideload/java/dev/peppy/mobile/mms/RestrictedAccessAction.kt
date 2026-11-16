package dev.peppy.mobile.mms

import android.content.Context
import androidx.compose.runtime.Composable
import dev.peppy.mobile.R

@Composable
internal fun RestrictedNotAllowedAccessControls(
    context: Context,
    @Suppress("UNUSED_PARAMETER") refresh: Int,
    onOpenGoogleMessagesLink: () -> Unit,
) {
    RestrictedMessageAccessStatus(RestrictedMessageAccess.NOT_ALLOWED)
    RestrictedAccessAction(context, onOpenGoogleMessagesLink)
}

@Composable
internal fun RestrictedAccessAction(context: Context, onOpenGoogleMessagesLink: () -> Unit) =
    RestrictedAccessCommand(context)

internal fun restrictedMessageAccessHelpResId(): Int = R.string.peppy_mms_restricted_help
