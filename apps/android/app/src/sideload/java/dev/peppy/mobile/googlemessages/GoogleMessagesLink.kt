package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.compose.runtime.Composable

object GoogleMessagesLink {
    @Composable
    fun Entry(onChanged: () -> Unit) = Unit

    fun available(context: Context): Boolean = LinkEngines.current.available

    fun onAppStart(context: Context) {
        GoogleMessagesLinkService.createNotificationChannel(context.applicationContext)
    }

    fun signInBlocked(context: Context): Boolean = false
}
