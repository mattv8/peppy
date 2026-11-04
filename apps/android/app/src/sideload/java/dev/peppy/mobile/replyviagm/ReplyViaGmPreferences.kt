package dev.peppy.mobile.replyviagm

import android.content.Context
import dev.peppy.mobile.gateway.GatewayPolicyHost

internal class ReplyViaGmPreferences(context: Context) {
    private val policy = GatewayPolicyHost(context.applicationContext)

    suspend fun enabled(): Boolean = policy.replyViaGoogleMessagesEnabled

    suspend fun setEnabled(value: Boolean) {
        policy.replyViaGoogleMessagesEnabled = value
    }
}
