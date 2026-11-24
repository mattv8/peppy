package dev.peppy.mobile.mms

import android.content.Context
import androidx.compose.runtime.Composable
import dev.peppy.mobile.R

internal fun restrictedMessageAccessVisible(): Boolean = true

@Composable
internal fun RestrictedNotAllowedAccessControls(
    context: Context,
    @Suppress("UNUSED_PARAMETER") refresh: Int,
) {
    RestrictedMessageAccessStatus(RestrictedMessageAccess.NOT_ALLOWED)
    RestrictedAccessAction(context)
}

@Composable
internal fun RestrictedAccessAction(context: Context) =
    RestrictedAccessCommand(context)

internal fun restrictedMessageAccessHelpResId(): Int = R.string.peppy_mms_restricted_help
