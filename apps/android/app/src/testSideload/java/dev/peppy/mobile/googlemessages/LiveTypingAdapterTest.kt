package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.peppy_mobile_bindings.NativeLiveAction
import uniffi.peppy_mobile_bindings.NativeLiveOutcome
import uniffi.peppy_mobile_bindings.NativeLiveSessionUpdate
import uniffi.peppy_mobile_bindings.NativeTypingState
import java.util.concurrent.Executor

class LiveTypingAdapterTest {
    @Test
    fun `only typed gateway intents with the current generation publish one effect`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)

        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, action = display(sequence = 2UL, deadline = 150UL)))
        adapter.consume(outcome(8UL, action = intent(sequence = 3UL, deadline = 150UL)))
        executor.runAll()

        assertEquals(1, effects.size)
        assertEquals("conversation", effects.single().conversationId)
        assertEquals(NativeTypingState.STARTED, effects.single().state)
        assertEquals(150UL, effects.single().deadlineMono)
    }

    @Test
    fun `retired generation session revision or token drops delayed intent`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, update = session(token = 9UL, revision = 2UL)))
        executor.runAll()

        assertEquals(emptyList<LiveTypingEffectIntent>(), effects)
    }

    @Test
    fun `rollback session update cannot revive retired authority`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session(token = 8UL, revision = 1UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 102UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, update = session(token = 9UL, revision = 2UL)))
        adapter.consume(outcome(7UL, update = session(token = 8UL, revision = 1UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 103UL, deadline = 150UL)))
        executor.runAll()

        assertEquals(emptyList<LiveTypingEffectIntent>(), effects)
    }

    @Test
    fun `equal session update does not clear sequence fences`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 102UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 102UL, deadline = 150UL)))
        executor.runAll()

        assertEquals(1, effects.size)
    }

    @Test
    fun `lower or equal sequence is admitted at most once`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 2UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 2UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 150UL)))
        executor.runAll()

        assertEquals(1, effects.size)
    }

    @Test
    fun `stopped intent retires queued start without a stop effect`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 102UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 103UL, deadline = 150UL, state = NativeTypingState.STOPPED)))
        executor.runAll()

        assertEquals(emptyList<LiveTypingEffectIntent>(), effects)
    }

    @Test
    fun `reverse executor order never publishes an older sequence`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 102UL, deadline = 150UL)))
        adapter.consume(outcome(7UL, action = intent(sequence = 104UL, deadline = 150UL)))
        executor.runLast()
        executor.runAll()

        assertEquals(1, effects.size)
    }

    @Test
    fun `expiration during delayed delivery suppresses the effect`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 101UL)))
        clock.now = 101UL
        executor.runAll()

        assertEquals(emptyList<LiveTypingEffectIntent>(), effects)
    }

    @Test
    fun `effect retains the absolute monotonic deadline`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 5_200UL)))
        clock.now = 1_000UL
        executor.runAll()

        assertEquals(5_200UL, effects.single().deadlineMono)
    }

    @Test
    fun `fresh long core deadline is retained for the native boundary to bound`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 15_100UL)))
        executor.runAll()

        assertEquals(15_100UL, effects.single().deadlineMono)
    }

    @Test
    fun `disable retires pending callbacks without replay`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 150UL)))
        adapter.disable()
        executor.runAll()

        assertEquals(emptyList<LiveTypingEffectIntent>(), effects)
    }

    @Test
    fun `published effect currency retires when the live incarnation is disabled`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 150UL)))
        executor.runAll()

        adapter.disable()

        assertEquals(false, effects.single().isCurrent.isCurrent())
    }

    private fun outcome(
        generation: ULong,
        update: NativeLiveSessionUpdate? = null,
        action: NativeLiveAction? = null,
    ) = NativeLiveOutcome(generation, update, action, null)

    private fun session(token: ULong = 8UL, revision: ULong = 1UL) =
        NativeLiveSessionUpdate("peer", null, token, revision)

    private fun intent(
        token: ULong = 8UL,
        sequence: ULong,
        deadline: ULong,
        state: NativeTypingState = NativeTypingState.STARTED,
    ) = NativeLiveAction.GatewayTypingIntent(
        "peer", token, sequence, "conversation", null, state, deadline,
    )

    private fun display(sequence: ULong, deadline: ULong) = NativeLiveAction.Display(
        "peer", 8UL, sequence, "conversation", null, NativeTypingState.STARTED, deadline,
    )

    private class FakeClock(var now: ULong) : MonotonicClock {
        override fun nowMs(): ULong = now
    }

    private class QueueExecutor : Executor {
        private val tasks = mutableListOf<Runnable>()

        override fun execute(command: Runnable) {
            tasks += command
        }

        fun runAll() {
            while (tasks.isNotEmpty()) tasks.removeAt(0).run()
        }

        fun runLast() {
            tasks.removeAt(tasks.lastIndex).run()
        }
    }
}
