package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Collections
import java.util.concurrent.Executor
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.atomic.AtomicInteger

class ReceiverWorkTest {
    @Test
    fun schedulesActualRemainingDeadlineBudgetAndClampsExpiredWork() {
        val delays = mutableListOf<Long>()
        fun scheduleAt(now: Long): Long {
            ReceiverWork.run(
                scheduler = ReceiverWork.Scheduler("test", Executor { it.run() }, { delay, _ ->
                    delays += delay
                    ReceiverWork.Cancellation {}
                }, Executor { it.run() }, { now }),
                receivedAtMillis = 100L,
                finish = {},
                work = {},
            )
            return delays.removeAt(delays.lastIndex)
        }

        assertEquals(7_000L, scheduleAt(100L))
        assertEquals(6_500L, scheduleAt(600L))
        assertEquals(0L, scheduleAt(7_101L))
    }

    @Test
    fun deadlineIncludesQueueWaitAndFinishesOnlyOnce() {
        val queued = mutableListOf<Runnable>()
        val deadlines = mutableListOf<Runnable>()
        val events = mutableListOf<String>()
        val scheduler = ReceiverWork.Scheduler(
            name = "test",
            worker = Executor { queued += it },
            deadline = { _, runnable ->
                deadlines += runnable
                ReceiverWork.Cancellation {}
            },
            recovery = Executor { it.run() },
            nowMillis = { 7_000L },
        )

        ReceiverWork.run(
            scheduler = scheduler,
            receivedAtMillis = 0L,
            finish = { events += "finish" },
            recovery = { events += "recovery" },
            work = { events += "work" },
        )

        deadlines.single().run()
        queued.single().run()

        assertEquals(listOf("recovery", "finish", "work"), events)
    }

    @Test
    fun rejectedWorkFinishesWithoutRunningOnCaller() {
        val events = mutableListOf<String>()
        val scheduler = ReceiverWork.Scheduler(
            name = "test",
            worker = Executor { throw java.util.concurrent.RejectedExecutionException() },
            deadline = { _, _ -> ReceiverWork.Cancellation {} },
            recovery = Executor { it.run() },
            nowMillis = { 0L },
        )

        ReceiverWork.run(
            scheduler = scheduler,
            receivedAtMillis = 0L,
            finish = { events += "finish" },
            recovery = { events += "recovery" },
            work = { events += "work" },
        )

        assertEquals(listOf("recovery", "finish"), events)
        assertFalse(events.contains("work"))
    }

    @Test
    fun completionCancelsItsDeadlineTask() {
        var cancelled = false
        val scheduler = ReceiverWork.Scheduler(
            name = "test",
            worker = Executor { it.run() },
            deadline = { _, _ -> ReceiverWork.Cancellation { cancelled = true } },
            recovery = Executor { it.run() },
            nowMillis = { 0L },
        )

        ReceiverWork.run(scheduler, 0L, {}, {}, {})

        assertTrue(cancelled)
    }

    @Test
    fun completionAndDeadlineRaceOnRealThreadsFinishAndRecoverAtMostOnce() {
        val workStarted = CountDownLatch(1)
        val releaseWork = CountDownLatch(1)
        val deadline = arrayOfNulls<Runnable>(1)
        val finished = AtomicInteger()
        val recovered = AtomicInteger()
        val workers = Executors.newSingleThreadExecutor()
        val recovery = Executors.newSingleThreadExecutor()
        val scheduler = ReceiverWork.Scheduler(
            "test", workers, { _, runnable -> deadline[0] = runnable; ReceiverWork.Cancellation {} }, recovery, { 0L },
        )
        try {
            ReceiverWork.run(scheduler, 0L, { finished.incrementAndGet() }, { recovered.incrementAndGet() }) {
                workStarted.countDown()
                releaseWork.await(2, TimeUnit.SECONDS)
            }
            assertTrue(workStarted.await(2, TimeUnit.SECONDS))
            val racers = Executors.newFixedThreadPool(2)
            try {
                racers.execute { deadline[0]!!.run() }
                racers.execute { releaseWork.countDown() }
            } finally {
                racers.shutdown()
                assertTrue(racers.awaitTermination(2, TimeUnit.SECONDS))
            }
            workers.shutdown()
            recovery.shutdown()
            assertTrue(workers.awaitTermination(2, TimeUnit.SECONDS))
            assertTrue(recovery.awaitTermination(2, TimeUnit.SECONDS))
            assertEquals(1, finished.get())
            assertTrue(recovered.get() in 0..1)
        } finally {
            releaseWork.countDown()
            workers.shutdownNow()
            recovery.shutdownNow()
        }
    }

    @Test
    fun saturated64SlotQueueRejectsWithoutExecutingWorkOnCaller() {
        val blocked = CountDownLatch(1)
        val started = CountDownLatch(1)
        val executor = ThreadPoolExecutor(1, 1, 1, TimeUnit.SECONDS, ArrayBlockingQueue(64), ThreadPoolExecutor.AbortPolicy())
        val caller = Thread.currentThread().name
        val ranOn = Collections.synchronizedList(mutableListOf<String>())
        try {
            executor.execute { started.countDown(); blocked.await(2, TimeUnit.SECONDS) }
            assertTrue(started.await(2, TimeUnit.SECONDS))
            repeat(64) { executor.execute {} }
            val scheduler = ReceiverWork.Scheduler("test", executor, { _, _ -> ReceiverWork.Cancellation {} }, Executor { it.run() }, { 0L })
            ReceiverWork.run(scheduler, 0L, {}, {}) { ranOn += Thread.currentThread().name }
            assertTrue(ranOn.isEmpty())
            assertEquals(64, executor.queue.size)
            assertTrue(executor.isShutdown.not())
            assertFalse(ranOn.contains(caller))
        } finally {
            blocked.countDown()
            executor.shutdownNow()
        }
    }

    @Test
    fun workFailureSubmitsRecoveryBeforeFinishing() {
        val events = mutableListOf<String>()
        val scheduler = ReceiverWork.Scheduler(
            "test", Executor { it.run() }, { _, _ -> ReceiverWork.Cancellation {} }, Executor { it.run() }, { 0L },
        )

        ReceiverWork.run(scheduler, 0L, { events += "finish" }, { events += "recovery" }) { throw IllegalStateException() }

        assertEquals(listOf("recovery", "finish"), events)
    }

    @Test
    fun recoveryFailureAndRecoveryRejectionStillFinishExactlyOnce() {
        val failureFinished = AtomicInteger()
        val failureScheduler = ReceiverWork.Scheduler(
            "test", Executor { it.run() }, { _, runnable -> runnable.run(); ReceiverWork.Cancellation {} }, Executor { it.run() }, { 0L },
        )
        ReceiverWork.run(failureScheduler, 0L, { failureFinished.incrementAndGet() }, { throw IllegalStateException() }) {}
        assertEquals(1, failureFinished.get())

        val rejectedFinished = AtomicInteger()
        val rejectedScheduler = ReceiverWork.Scheduler(
            "test", Executor { throw RejectedExecutionException() }, { _, _ -> ReceiverWork.Cancellation {} }, Executor { throw RejectedExecutionException() }, { 0L },
        )
        ReceiverWork.run(rejectedScheduler, 0L, { rejectedFinished.incrementAndGet() }, {}) {}
        assertEquals(1, rejectedFinished.get())
    }
}
