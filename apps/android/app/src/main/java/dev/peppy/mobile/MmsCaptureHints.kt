package dev.peppy.mobile

import android.content.Context
import androidx.work.ExistingWorkPolicy

/**
 * Coalesces durable provider hints without querying WorkManager's asynchronous work state.
 *
 * The completion fence supplies one APPEND_OR_REPLACE successor through WorkManager's completion
 * race; it resets when that managed successor begins. The preference bit survives process death.
 */
internal class MmsCaptureHintScheduler {
    private var running = false
    private var passQueued = false
    private var completionFenced = false

    @Synchronized
    fun requestNormal(sink: MmsCaptureScheduleSink) = queue(sink)

    @Synchronized
    fun requestHint(sink: MmsCaptureScheduleSink) = queue(sink)

    private fun queue(sink: MmsCaptureScheduleSink) {
        if (passQueued) return
        val policy = if (running || completionFenced) ExistingWorkPolicy.APPEND_OR_REPLACE else ExistingWorkPolicy.KEEP
        sink.enqueue(policy, delayed = true)
        passQueued = true
    }

    @Synchronized
    fun beginPass(managedWork: Boolean) {
        if (!managedWork) {
            if (!running) {
                passQueued = false
                completionFenced = false
            }
            return
        }
        running = true
        completionFenced = false
        passQueued = false
    }

    @Synchronized
    fun finishPass(managedWork: Boolean) {
        if (!managedWork) return
        running = false
        completionFenced = true
    }

    @Synchronized
    fun replace(sink: MmsCaptureScheduleSink) {
        sink.enqueue(ExistingWorkPolicy.REPLACE, delayed = false)
        passQueued = true
    }

    @Synchronized
    fun requestContinuation(actualMore: Boolean, sink: MmsCaptureScheduleSink) {
        if (actualMore) queue(sink)
    }
}

internal fun interface MmsCaptureScheduleSink {
    fun enqueue(policy: ExistingWorkPolicy, delayed: Boolean)
}

/** Durable bit shared by observer and WAP hints. All read-modify-write operations use [LOCK]. */
internal object MmsCaptureHints {
    internal const val PREFERENCES = "peppy-mms-capture-hints"
    internal const val PENDING = "provider-hint-pending.v1"
    private val LOCK = Any()

    fun record(context: Context) = synchronized(LOCK) {
        inProcessPending = !context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .edit().putBoolean(PENDING, true).commit()
    }

    /** All provider-change authority is durable, with a process-local fallback for failed commits. */
    fun consumeProviderChange(context: Context): Boolean = synchronized(LOCK) {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val pending = preferences.getBoolean(PENDING, false)
        val fallback = inProcessPending
        inProcessPending = false
        if (pending && !preferences.edit().putBoolean(PENDING, false).commit()) {
            inProcessPending = true
        }
        pending || fallback
    }

    private var inProcessPending = false
}
