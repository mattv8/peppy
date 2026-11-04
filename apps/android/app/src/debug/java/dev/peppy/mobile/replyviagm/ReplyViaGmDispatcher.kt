package dev.peppy.mobile.replyviagm

import android.content.Context
import dev.peppy.mobile.sms.DispatchSummary
import dev.peppy.mobile.telephony.SimRoute
import uniffi.peppy_mobile_bindings.NativeClientInterface

internal class ReplyViaGmDispatcher(context: Context, client: NativeClientInterface, routes: List<SimRoute> = emptyList()) {
    fun dispatch(limit: Int): DispatchSummary = DispatchSummary(0, 0, 0, false)
}
