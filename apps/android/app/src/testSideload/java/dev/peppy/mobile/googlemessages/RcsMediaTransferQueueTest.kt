package dev.peppy.mobile.googlemessages

import java.util.ArrayDeque
import java.util.concurrent.Executor
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RcsMediaTransferQueueTest {
    @Test fun `upload and download never overlap`() {
        val executor = QueuedExecutor()
        val queue = RcsMediaTransferQueue(executor)
        val events = mutableListOf<String>()
        var inFlight = 0
        var maximumInFlight = 0

        assertTrue(queue.submit("upload") {
            inFlight += 1
            maximumInFlight = maxOf(maximumInFlight, inFlight)
            events += "upload"
            inFlight -= 1
        })
        assertTrue(queue.submit("download") {
            inFlight += 1
            maximumInFlight = maxOf(maximumInFlight, inFlight)
            events += "download"
            inFlight -= 1
        })

        executor.runNext()
        executor.runNext()

        assertEquals(listOf("upload", "download"), events)
        assertEquals(1, maximumInFlight)
        assertNull(queue.current())
    }

    @Test fun `stopped callbacks do not publish`() {
        val executor = QueuedExecutor()
        val queue = RcsMediaTransferQueue(executor)
        var lease: RcsMediaTransferQueue.Lease? = null
        var published = false

        queue.submit("download") { lease = it }
        executor.runNext()
        queue.stop()

        assertFalse(lease!!.isCurrent())
        assertFalse(lease!!.publish { published = true })
        assertFalse(published)
    }

    @Test fun `duplicate durable job does not run twice`() {
        val executor = QueuedExecutor()
        val queue = RcsMediaTransferQueue(executor)
        var runs = 0

        assertTrue(queue.submit("message-1/0") { runs += 1 })
        assertFalse(queue.submit("message-1/0") { runs += 1 })
        executor.runNext()

        assertEquals(1, runs)
    }

    @Test fun `fresh incarnation accepts fresh work while old callbacks stay fenced`() {
        val oldExecutor = QueuedExecutor()
        val oldQueue = RcsMediaTransferQueue(oldExecutor)
        var stale: RcsMediaTransferQueue.Lease? = null
        oldQueue.submit("message-1/0") { stale = it }
        oldExecutor.runNext()
        oldQueue.stop()

        val newExecutor = QueuedExecutor()
        val newQueue = RcsMediaTransferQueue(newExecutor)
        var published = false
        assertTrue(newQueue.submit("message-1/0") { lease -> lease.publish { published = true } })
        newExecutor.runNext()

        assertFalse(stale!!.publish { published = false })
        assertTrue(published)
    }

    private class QueuedExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()

        override fun execute(command: Runnable) {
            tasks.addLast(command)
        }

        fun runNext() {
            check(tasks.isNotEmpty()) { "No queued task" }
            tasks.removeFirst().run()
        }
    }
}
