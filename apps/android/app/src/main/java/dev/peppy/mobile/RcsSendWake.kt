package dev.peppy.mobile

/** Optional sideload hook for newly journaled gateway commands. */
object RcsSendWake {
    @Volatile
    var listener: (() -> Unit)? = null

    fun notifyCommandsJournaled() {
        listener?.invoke()
    }
}
