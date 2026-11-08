package dev.peppy.mobile.googlemessages

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** Serializes owner-confirmed destructive link operations across panel instances. */
internal object LinkOperationLock {
    private val mutableInProgress = MutableStateFlow(false)

    val inProgress: StateFlow<Boolean> = mutableInProgress

    fun tryAcquire(): Boolean = synchronized(this) {
        if (mutableInProgress.value) return false
        mutableInProgress.value = true
        true
    }

    fun release() = synchronized(this) {
        mutableInProgress.value = false
    }
}
