package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test
import java.util.concurrent.Executor

class RcsNativeSendWorkTest {
    @Test fun `disabled work does not read candidates or run native sends`() {
        var reads = 0
        var sends = 0

        RcsNativeSendWork(
            enabled = { false },
            candidates = { reads += 1; listOf(command("one")) },
            execute = { sends += 1 },
            current = { true },
            executor = directExecutor,
        ).start()

        assertEquals(0, reads)
        assertEquals(0, sends)
    }

    @Test fun `empty first pass can run a later candidate`() {
        val executor = QueueExecutor()
        var reads = 0
        val sent = mutableListOf<String>()
        val work = RcsNativeSendWork(
            enabled = { true },
            candidates = { reads += 1; if (reads == 1) emptyList() else listOf(command("later")) },
            execute = { sent += it.commandId },
            current = { true },
            executor = executor,
        )

        work.start()
        executor.runNext()
        work.start()
        executor.runNext()

        assertEquals(listOf("later"), sent)
    }

    @Test fun `overlapping starts coalesce into one serial followup pass`() {
        val executor = QueueExecutor()
        val sent = mutableListOf<String>()
        lateinit var work: RcsNativeSendWork
        work = RcsNativeSendWork(
            enabled = { true },
            candidates = { listOf(command("candidate")) },
            execute = {
                sent += it.commandId
                if (sent.size == 1) {
                    work.start()
                    work.start()
                }
            },
            current = { true },
            executor = executor,
        )

        work.start()
        executor.runNext()

        assertEquals(listOf("candidate", "candidate"), sent)
        assertEquals(0, executor.size)
    }

    @Test fun `a carrier handoff ends the batch so no later command stales its proof`() {
        val executor = QueueExecutor()
        val sent = mutableListOf<String>()
        var handedOff = false
        lateinit var work: RcsNativeSendWork
        work = RcsNativeSendWork(
            enabled = { true },
            candidates = { listOf(command("carrier-only"), command("blocked")) },
            execute = {
                sent += it.commandId
                handedOff = it.commandId == "carrier-only"
                // A wake that arrives mid-batch must not re-run the batch before the carrier acts.
                work.start()
            },
            current = { true },
            executor = executor,
            carrierHandoffRequested = { handedOff.also { handedOff = false } },
        )

        work.start()
        executor.runNext()

        assertEquals(listOf("carrier-only"), sent)
        assertEquals(0, executor.size)
    }

    @Test fun `work resumes with the next wake after a carrier handoff`() {
        val executor = QueueExecutor()
        val sent = mutableListOf<String>()
        var pending = listOf(command("carrier-only"), command("blocked"))
        val work = RcsNativeSendWork(
            enabled = { true },
            candidates = { pending },
            execute = { sent += it.commandId },
            current = { true },
            executor = executor,
            carrierHandoffRequested = { sent.last() == "carrier-only" },
        )
        work.start()
        executor.runNext()
        pending = listOf(command("blocked"))

        work.start()
        executor.runNext()

        assertEquals(listOf("carrier-only", "blocked"), sent)
    }

    @Test fun `enabled work sends candidates sequentially until retired`() {
        val sent = mutableListOf<String>()
        lateinit var work: RcsNativeSendWork
        work = RcsNativeSendWork(
            enabled = { true },
            candidates = { listOf(command("one"), command("two")) },
            execute = { command -> sent += command.commandId; if (command.commandId == "one") work.stop() },
            current = { true },
            executor = directExecutor,
        )

        work.start()

        assertEquals(listOf("one"), sent)
    }

    private fun command(id: String) = RcsNativeCommand(id, "conversation-$id", "recipient-$id")

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        val size get() = tasks.size
        override fun execute(command: Runnable) { tasks += command }
        fun runNext() = tasks.removeFirst().run()
    }

    private companion object {
        val directExecutor = Executor { runnable -> runnable.run() }
    }
}
