package dev.peppy.mobile.restrictedsetup

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput

internal class RestrictedSetupReplyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            ACTION_REPLY -> {
                val characters = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(RESULT_KEY)?.toString()?.toCharArray() ?: CharArray(0)
                when (RestrictedSetupCodeInbox.deliver(trimAsciiWhitespace(characters))) {
                    CodeSubmitResult.ACCEPTED, CodeSubmitResult.INVALID_FORMAT -> Unit
                    else -> RestrictedSetupRuntime.notifier(context).show(notice(intent))
                }
            }
            ACTION_CANCEL -> RestrictedSetupRuntime.controller(context).cancel()
        }
    }

    private fun trimAsciiWhitespace(value: CharArray): CharArray {
        var start = 0
        var end = value.size
        while (start < end && value[start].isAsciiWhitespace()) start++
        while (end > start && value[end - 1].isAsciiWhitespace()) end--
        val trimmed = value.copyOfRange(start, end)
        value.fill('\u0000')
        return trimmed
    }

    private fun Char.isAsciiWhitespace(): Boolean = this == ' ' || this in '\t'..'\r'

    private fun notice(intent: Intent): RestrictedSetupNotice =
        intent.getStringExtra(EXTRA_NOTICE)?.let { runCatching { RestrictedSetupNotice.valueOf(it) }.getOrNull() }
            ?: RestrictedSetupNotice.CODE_PROMPT

    internal companion object {
        const val ACTION_REPLY = "dev.peppy.mobile.restrictedsetup.REPLY"
        const val ACTION_CANCEL = "dev.peppy.mobile.restrictedsetup.CANCEL"
        const val RESULT_KEY = "restricted_setup_code"
        const val EXTRA_NOTICE = "restricted_setup_notice"
    }
}
