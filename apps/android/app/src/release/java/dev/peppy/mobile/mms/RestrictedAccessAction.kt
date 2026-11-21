package dev.peppy.mobile.mms

import android.content.Context
import androidx.compose.runtime.Composable
import dev.peppy.mobile.R

internal fun restrictedMessageAccessVisible(): Boolean = false

@Composable
internal fun RestrictedNotAllowedAccessControls(
    @Suppress("UNUSED_PARAMETER") context: Context,
    @Suppress("UNUSED_PARAMETER") refresh: Int,
    @Suppress("UNUSED_PARAMETER") onOpenGoogleMessagesLink: () -> Unit,
) = Unit

internal fun restrictedMessageAccessHelpResId(): Int = R.string.peppy_mms_restricted_play_help
