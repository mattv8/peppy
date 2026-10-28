package dev.peppy.mobile

import android.os.SystemClock
import android.util.Log
import androidx.annotation.VisibleForTesting
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Bounded work that may outlive an Android broadcast, without ever running core work on its caller. */
object ReceiverWork {
    const val DEADLINE_MILLIS = 7_000L

    internal fun interface Cancellation {
        fun cancel()
    }

    internal class Scheduler(
        val name: String,
        val worker: Executor,
        val deadline: (Long, Runnable) -> Cancellation,
        val recovery: Executor,
        val nowMillis: () -> Long,
    )

    private val owned = ThreadPoolExecutor(
        1,
        1,
        30,
        TimeUnit.SECONDS,
        ArrayBlockingQueue(64),
        { runnable -> Thread(runnable, "peppy-receiver").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val recoveryOwned = ThreadPoolExecutor(
        1,
        1,
        30,
        TimeUnit.SECONDS,
        ArrayBlockingQueue(32),
        { runnable -> Thread(runnable, "peppy-receiver-recovery").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val evidenceOwned = ThreadPoolExecutor(
        1,
        1,
        30,
        TimeUnit.SECONDS,
        ArrayBlockingQueue(64),
        { runnable -> Thread(runnable, "peppy-callback-evidence").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val callbackApplyOwned = ThreadPoolExecutor(
        1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(64),
        { runnable -> Thread(runnable, "peppy-callback-apply").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val deadlines = ScheduledThreadPoolExecutor(1) { runnable ->
        Thread(runnable, "peppy-receiver-deadline").apply { isDaemon = true }
    }.apply { setRemoveOnCancelPolicy(true) }

    @VisibleForTesting
    @Volatile
    internal var executor: Executor = owned

    @VisibleForTesting
    @Volatile
    internal var evidenceExecutor: Executor = evidenceOwned

    @VisibleForTesting
    @Volatile
    internal var callbackApplyExecutor: Executor = callbackApplyOwned

    private val coreScheduler = Scheduler(
        name = "incoming capture",
        worker = Executor { command -> executor.execute(command) },
        deadline = { delay, command -> deadlines.schedule(command, delay, TimeUnit.MILLISECONDS).let { task -> Cancellation { task.cancel(false) } } },
        recovery = recoveryOwned,
        nowMillis = SystemClock::elapsedRealtime,
    )
    internal val evidenceScheduler = Scheduler(
        name = "callback evidence",
        worker = Executor { command -> evidenceExecutor.execute(command) },
        deadline = { delay, command -> deadlines.schedule(command, delay, TimeUnit.MILLISECONDS).let { task -> Cancellation { task.cancel(false) } } },
        recovery = recoveryOwned,
        nowMillis = SystemClock::elapsedRealtime,
    )

    internal fun run(
        scheduler: Scheduler = coreScheduler,
        receivedAtMillis: Long = scheduler.nowMillis(),
        finish: () -> Unit,
        recovery: () -> Unit = {},
        work: () -> Unit,
    ) {
        val finished = AtomicBoolean(false)
        val recovered = AtomicBoolean(false)
        val cancellation = AtomicReference<Cancellation?>()
        fun claimFinish(): Boolean {
            if (finished.compareAndSet(false, true)) {
                cancellation.get()?.cancel()
                return true
            }
            return false
        }
        fun finishOnce(): Boolean = claimFinish().also { if (it) finish() }
        fun recoverOnce() {
            if (!recovered.compareAndSet(false, true)) return
            try {
                scheduler.recovery.execute {
                    try {
                        recovery()
                    } catch (error: Exception) {
                        diagnose("receiver recovery failed: ${error.javaClass.simpleName}")
                    }
                }
            } catch (_: RejectedExecutionException) {
                diagnose("receiver recovery rejected")
            }
        }
        fun recoverThenFinish() {
            if (claimFinish()) {
                recoverOnce()
                finish()
            }
        }
        val delay = deadlineDelayMillis(receivedAtMillis, scheduler.nowMillis())
        val deadline = scheduler.deadline(delay) {
            recoverThenFinish()
        }
        cancellation.set(deadline)
        if (finished.get()) deadline.cancel()
        try {
            scheduler.worker.execute {
                try {
                    work()
                } catch (error: Exception) {
                    diagnose("${scheduler.name} failed: ${error.javaClass.simpleName}")
                    recoverThenFinish()
                } finally {
                    finishOnce()
                }
            }
        } catch (_: RejectedExecutionException) {
            diagnose("${scheduler.name} rejected")
            recoverThenFinish()
        }
    }

    /** Callback reconciliation is replayable and must not consume incoming-capture capacity. */
    internal fun submitCallbackApply(work: () -> Unit, recovery: () -> Unit) {
        try {
            callbackApplyExecutor.execute {
                try {
                    work()
                } catch (error: Exception) {
                    diagnose("callback apply failed: ${error.javaClass.simpleName}")
                    handoffRecovery(recovery)
                }
            }
        } catch (_: RejectedExecutionException) {
            diagnose("callback apply rejected")
            handoffRecovery(recovery)
        }
    }

    private fun handoffRecovery(recovery: () -> Unit) {
        try {
            recoveryOwned.execute {
                try {
                    recovery()
                } catch (error: Exception) {
                    diagnose("receiver recovery failed: ${error.javaClass.simpleName}")
                }
            }
        } catch (_: RejectedExecutionException) {
            diagnose("receiver recovery rejected")
        }
    }

    internal fun diagnose(message: String) {
        runCatching { Log.w("PeppyReceiver", message) }
    }

    @VisibleForTesting
    internal fun deadlineDelayMillis(receivedAtMillis: Long, nowMillis: Long): Long =
        (DEADLINE_MILLIS - (nowMillis - receivedAtMillis)).coerceAtLeast(0L)
}
