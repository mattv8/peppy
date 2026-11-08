package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.compose.runtime.Composable

object GoogleMessagesLink {
    @Composable
    fun Entry(onChanged: () -> Unit) = Unit

    fun available(context: Context): Boolean = false

    fun onAppStart(context: Context) = Unit
}
