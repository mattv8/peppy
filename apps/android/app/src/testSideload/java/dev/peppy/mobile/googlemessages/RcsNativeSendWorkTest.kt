package dev.peppy.mobile.googlemessages

import org.junit.Assert.assertEquals
import org.junit.Test
import dev.peppy.mobile.gateway.revocationCheckedCandidates
import dev.peppy.mobile.gateway.cachedProducerRetirement
import java.util.concurrent.Executor

class RcsNativeSendWorkTest {
    @Test fun `new command after group work is retired using cached roster`() {
        var loads = 0
        var pending = listOf(command("first"))
        val retire = cachedProducerRetirement(
            load = { loads += 1; listOf("revoked") },
            retire = { ids -> if ("revoked" in ids) pending = emptyList() },
        )
        assertEquals(emptyList<RcsNativeCommand>(), revocationCheckedCandidates({ pending }, retire))
        pending = listOf(command("arrived-after-group-create"))
        assertEquals(emptyList<RcsNativeCommand>(), revocationCheckedCandidates({ pending }, retire))
        assertEquals(1, loads)
    }

    @Test fun `revoked producer is retired before native work admits a journaled command`() {
        var pending = listOf(command("revoked"))
        var failedBeforeSubmission = 0
        var goSendEffects = 0
        val work = RcsNativeSendWork(
            enabled = { true },
            candidates = {
                revocationCheckedCandidates(
                    read = { pending },
                    retire = {
                        failedBeforeSubmission += pending.size
                        pending = emptyList()
                        true
                    },
                )
            },
            execute = { goSendEffects += 1 },
            current = { true },
            executor = directExecutor,
        )
        work.start()
        assertEquals(0, goSendEffects)
        assertEquals(1, failedBeforeSubmission)
    }

    @Test fun `unavailable roster retains previous candidates and empty work skips lookup`() {
        var lookups = 0
        assertEquals(emptyList<RcsNativeCommand>(), revocationCheckedCandidates(
            read = { emptyList<RcsNativeCommand>() },
            retire = { lookups += 1; true },
        ))
        val candidate = command("one")
        assertEquals(listOf(candidate), revocationCheckedCandidates(
            read = { listOf(candidate) },
            retire = { lookups += 1; false },
        ))
        assertEquals(1, lookups)
    }

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
