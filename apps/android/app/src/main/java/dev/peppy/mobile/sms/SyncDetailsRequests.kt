package dev.peppy.mobile.sms

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

internal object SyncDetailsRequests {
    private val pending = MutableStateFlow(false)
    val state: StateFlow<Boolean> = pending.asStateFlow()

    fun request() { pending.value = true }
    fun consume() { pending.value = false }
}
