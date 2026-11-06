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

        assertEquals(listOf(LiveTypingEffectIntent("conversation", NativeTypingState.STARTED, 50UL)), effects)
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
    fun `remaining validity is recalculated immediately before the effect`() {
        val clock = FakeClock(100UL)
        val executor = QueueExecutor()
        val effects = mutableListOf<LiveTypingEffectIntent>()
        val adapter = LiveTypingAdapter(clock, executor) { effects += it }
        adapter.activate(7UL)
        adapter.consume(outcome(7UL, update = session()))
        adapter.consume(outcome(7UL, action = intent(sequence = 1UL, deadline = 5_200UL)))
        clock.now = 1_000UL
        executor.runAll()

        assertEquals(4_200UL, effects.single().remainingValidityMs)
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

    private fun outcome(
        generation: ULong,
        update: NativeLiveSessionUpdate? = null,
        action: NativeLiveAction? = null,
    ) = NativeLiveOutcome(generation, update, action, null)

    private fun session(token: ULong = 8UL, revision: ULong = 1UL) =
        NativeLiveSessionUpdate("peer", null, token, revision)

    private fun intent(sequence: ULong, deadline: ULong) = NativeLiveAction.GatewayTypingIntent(
        "peer", 8UL, sequence, "conversation", NativeTypingState.STARTED, deadline,
    )

    private fun display(sequence: ULong, deadline: ULong) = NativeLiveAction.Display(
        "peer", 8UL, sequence, "conversation", NativeTypingState.STARTED, deadline,
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
    }
}
