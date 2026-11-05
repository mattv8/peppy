package dev.peppy.mobile.restrictedsetup

internal enum class RestrictedSetupNotice { SEARCHING, NETWORK_NOT_ALLOWED, CODE_PROMPT, CODE_INVALID, CODE_REJECTED, WORKING, DONE, FAILED }

internal interface RestrictedSetupNotifier {
    fun canPrompt(): Boolean
    fun show(notice: RestrictedSetupNotice)
    fun clear()
}

internal fun interface RestrictedSetupWirelessReadback { suspend fun isOn(): Boolean? }

internal object RestrictedSetupCodeInbox {
    @Volatile var sink: ((CharArray) -> CodeSubmitResult)? = null

    fun deliver(code: CharArray): CodeSubmitResult {
        val target = sink
        return if (target == null) {
            code.fill('\u0000')
            CodeSubmitResult.NOT_WAITING
        } else {
            target(code)
        }
    }
}
