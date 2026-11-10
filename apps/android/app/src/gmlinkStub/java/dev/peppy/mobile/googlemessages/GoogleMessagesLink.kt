package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.compose.runtime.Composable

object GoogleMessagesLink {
    @Composable
    fun Entry(
        onChanged: () -> Unit,
        notice: String?,
        onNoticeChange: (String?) -> Unit,
        onManage: () -> Unit,
        dismissedSeq: Long?,
        onDismissedSeqChange: (Long?) -> Unit,
        notificationAsked: Boolean,
        onNotificationAskedChange: (Boolean) -> Unit,
    ) = Unit

    @Composable
    fun Details(
        onBack: () -> Unit,
        onChanged: () -> Unit,
        notice: String?,
        onNoticeChange: (String?) -> Unit,
        dismissedSeq: Long?,
        onDismissedSeqChange: (Long?) -> Unit,
        notificationAsked: Boolean,
        onNotificationAskedChange: (Boolean) -> Unit,
    ) = Unit

    fun available(context: Context): Boolean = false

    fun onAppStart(context: Context) = Unit
}
