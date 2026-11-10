package dev.peppy.mobile.googlemessages

import java.util.concurrent.Executor
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsTargetedBootstrapDrainTest {
    @Test fun `offers coalesce and run once as one key`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; RcsBootstrapResult.COMPLETE }

        drain.offer("google-a")
        drain.offer("google-a")
        drain.offer("google-a")
        assertEquals(1, executor.size)
        executor.runAll()

        assertEquals(listOf("google-a"), runs)
        drain.offer("google-a")
        executor.runAll()
        assertEquals(2, runs.size)
        assertEquals("google-a", runs.last())
    }

    @Test fun `at most one drain task is queued and one pass runs every key in turn`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; RcsBootstrapResult.COMPLETE }

        drain.offer("a")
        drain.offer("b")
        drain.offer("c")

        assertEquals(1, executor.size)
        executor.runAll()
        assertEquals(listOf("a", "b", "c"), runs)
    }

    @Test fun `an arrival while in flight is coalesced and reruns once`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        lateinit var drain: RcsTargetedBootstrapDrain
        drain = newDrain(executor) { id ->
            runs += id
            if (runs.size == 1) drain.offer("google-a")
            RcsBootstrapResult.COMPLETE
        }
        drain.offer("google-a")

        executor.runAll()

        assertEquals(listOf("google-a", "google-a"), runs)
        executor.runAll()
        assertEquals(2, runs.size)
    }

    @Test fun `transient refusal re-arms the key until the next trigger and never spins`() {
        val executor = QueueExecutor()
        val outcomes = ArrayDeque(listOf(RcsBootstrapResult.TRANSIENT, RcsBootstrapResult.COMPLETE))
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; outcomes.removeFirst() }

        drain.offer("google-a")
        executor.runAll()
        assertEquals(1, runs.size)

        executor.runAll()
        assertEquals("parked key must not run again in the same trigger", 1, runs.size)

        drain.offer("google-a")
        executor.runAll()
        assertEquals(listOf("google-a", "google-a"), runs)
    }

    @Test fun `kick retries a parked key on an existing trigger`() {
        val executor = QueueExecutor()
        val outcomes = ArrayDeque(listOf(RcsBootstrapResult.TRANSIENT, RcsBootstrapResult.COMPLETE))
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; outcomes.removeFirst() }

        drain.offer("google-a")
        executor.runAll()
        drain.kick()
        executor.runAll()

        assertEquals(listOf("google-a", "google-a"), runs)
    }

    @Test fun `terminal refusal removes the key`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; RcsBootstrapResult.TERMINAL_REFUSAL }

        drain.offer("google-a")
        executor.runAll()
        drain.kick()
        executor.runAll()

        assertEquals(listOf("google-a"), runs)
    }

    @Test fun `a thrown bootstrap is a transient refusal and parks the key`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; error("synthetic") }

        drain.offer("google-a")
        executor.runAll()
        executor.runAll()

        assertEquals(listOf("google-a"), runs)
    }

    @Test fun `stopped pass keeps keys and resumes on the next trigger`() {
        val executor = QueueExecutor()
        var stop = true
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; if (stop) RcsBootstrapResult.STOPPED else RcsBootstrapResult.COMPLETE }

        drain.offer("google-a")
        executor.runAll()
        stop = false
        drain.kick()
        executor.runAll()

        assertEquals(listOf("google-a", "google-a"), runs)
    }

    @Test fun `the sixty-fifth key evicts the oldest inserted key`() {
        val executor = QueueExecutor()
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; RcsBootstrapResult.COMPLETE }

        (0..63).forEach { drain.offer("google-$it") }
        drain.offer("google-64")
        executor.runAll()

        assertEquals(64, runs.size)
        assertTrue("google-0" !in runs)
        assertEquals("google-1", runs.first())
        assertEquals("google-64", runs.last())
    }

    @Test fun `a fresh offer resets the transient attempt budget`() {
        val executor = QueueExecutor()
        val outcomes = ArrayDeque(List(4) { RcsBootstrapResult.TRANSIENT } + RcsBootstrapResult.TRANSIENT)
        val runs = mutableListOf<String>()
        val drain = newDrain(executor) { id -> runs += id; outcomes.removeFirst() }

        drain.offer("google-a")
        repeat(3) { executor.runAll(); drain.kick() }
        executor.runAll()
        drain.offer("google-a")
        executor.runAll()

        assertEquals(5, runs.size)
        drain.kick()
        executor.runAll()
        assertEquals(6, runs.size)
    }

    private fun newDrain(executor: Executor, run: (String) -> RcsBootstrapResult) =
        RcsTargetedBootstrapDrain(executor, alive = { true }, run = run)

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        val size: Int get() = tasks.size
        override fun execute(command: Runnable) { tasks += command }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }
}
