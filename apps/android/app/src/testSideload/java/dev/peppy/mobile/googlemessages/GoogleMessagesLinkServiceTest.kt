package dev.peppy.mobile.googlemessages

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleMessagesLinkServiceTest {
    @After
    fun cleanUp() {
        GoogleMessagesLinkService.stopActiveWorker()
        LinkRuntime.resetForTest()
        LinkRuntimeTestHooks.engine = null
        LinkRuntimeTestHooks.storeFactory = null
        GoogleMessagesLinkService.testHooks = ServiceTestHooks()
    }

    @Test
    fun `cancel before run entry closes the handle and native run rejects admission`() {
        val handle = FenceHandle()
        val beforeRun = CountDownLatch(1)
        val interruptObserved = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(beforeRun = {
            beforeRun.countDown()
            var closed = false
            while (!closed) {
                try {
                    handle.closeCalled.await()
                    closed = true
                } catch (_: InterruptedException) {
                    interruptObserved.countDown()
                }
            }
        })
        installEngine(handle)
        val controller = startService()
        assertTrue(beforeRun.await(1, TimeUnit.SECONDS))

        controller.destroy()

        assertTrue(interruptObserved.await(1, TimeUnit.SECONDS))
        assertTrue(handle.closed)
        assertTrue(handle.runRejected.await(1, TimeUnit.SECONDS))
        assertEquals(1, handle.runCalls.get())
    }

    @Test
    fun `old service destroy cannot close the new service handle`() {
        val oldHandle = BlockingRunHandle()
        val newHandle = BlockingRunHandle()
        installEngine(oldHandle, newHandle)
        val oldController = startService()
        assertTrue(oldHandle.runEntered.await(1, TimeUnit.SECONDS))

        val newController = startService()
        assertTrue(newHandle.runEntered.await(1, TimeUnit.SECONDS))
        oldController.destroy()

        assertTrue(oldHandle.closed)
        assertFalse(newHandle.closed)
        newController.destroy()
        oldHandle.allowRunReturn.countDown()
        newHandle.allowRunReturn.countDown()
    }

    @Test
    fun `pairing wait uses monotonic clock and covers the full native budget`() {
        val handle = StatusHandle(PAIRING_STATUS)
        installEngine(handle)
        var elapsed = 0L
        val finished = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            elapsedRealtime = { elapsed },
            sleeper = { elapsed += 30_000L },
            workerFinished = { finished.countDown() },
        )
        val attempt = LinkRuntime.tryBeginPairing()!!

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertEquals(390_000L, elapsed)
        assertTrue(LinkRuntime.endPairing(attempt))
    }

    @Test
    fun `conn ready resets retry backoff and run refused terminates without hot loop`() {
        val handle = RetryHandle()
        installEngine(handle)
        val delays = mutableListOf<Long>()
        val finished = CountDownLatch(1)
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            sleeper = { delay ->
                delays += delay
                if (delays.size == 1) GoogleMessagesLinkService.onLinkEvent("conn", "ready")
            },
            workerFinished = { finished.countDown() },
        )

        startService()

        assertTrue(finished.await(1, TimeUnit.SECONDS))
        assertEquals(listOf(30_000L, 30_000L), delays)
        assertEquals(3, handle.runCalls.get())
    }

    @Test
    fun `destroyed service refresh cannot publish after a held status read`() {
        val oldHandle = HeldRefreshHandle()
        val newHandle = BlockingRunHandle()
        installEngine(oldHandle, newHandle)
        val publishes = mutableListOf<GoogleMessagesLinkService>()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(
            notificationExecutor = Executor(Runnable::run),
            notificationPublished = { service -> synchronized(publishes) { publishes += service } },
        )
        val oldController = startService()
        val oldService = oldController.get()
        assertTrue(oldHandle.runEntered.await(1, TimeUnit.SECONDS))
        val beforeRefresh = synchronized(publishes) { publishes.count { it === oldService } }

        val refresh = Thread { GoogleMessagesLinkService.onLinkEvent("conn", "ready") }.also { it.start() }
        assertTrue(oldHandle.refreshStatusEntered.await(1, TimeUnit.SECONDS))
        val newController = startService()
        assertTrue(newHandle.runEntered.await(1, TimeUnit.SECONDS))
        oldHandle.allowRefreshStatus.countDown()
        assertTrue(oldHandle.refreshStatusReturned.await(1, TimeUnit.SECONDS))
        refresh.join(1_000)

        assertEquals(beforeRefresh, synchronized(publishes) { publishes.count { it === oldService } })
        oldController.destroy()
        newController.destroy()
        oldHandle.allowRunReturn.countDown()
        newHandle.allowRunReturn.countDown()
    }

    @Test
    fun `event notification refresh is submitted to the serialized executor`() {
        val executor = QueueExecutor()
        GoogleMessagesLinkService.testHooks = ServiceTestHooks(notificationExecutor = executor)
        installEngine(StatusHandle(UNLINKED_STATUS))
        val controller = Robolectric.buildService(GoogleMessagesLinkService::class.java).create()

        GoogleMessagesLinkService.onLinkEvent("conn", "ready")

        assertEquals(1, executor.size)
        controller.destroy()
    }

    private fun startService() = Robolectric.buildService(GoogleMessagesLinkService::class.java)
        .create()
        .startCommand(0, 1)

    private fun installEngine(vararg handles: LinkHandle) {
        val next = ArrayDeque(handles.toList())
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost): LinkHandle = next.removeFirst()
        }
        LinkRuntimeTestHooks.storeFactory = { EmptyStore }
    }

    private object EmptyStore : LinkStateStorage {
        override fun read(): ByteArray = ByteArray(0)
        override fun write(state: ByteArray) = Unit
        override fun wipe() = Unit
    }

    private open class StatusHandle(private val currentStatus: String) : LinkHandle {
        @Volatile var closed = false
        override fun pair(cookiesJson: ByteArray) = Unit
        override fun relink() = Unit
        override fun run() = Unit
        override fun checkHealth(): Long = -1
        override fun unlink() = Unit
        override fun stop() = Unit
        open override fun close() { closed = true }
        override fun status(): String = currentStatus
    }

    private class FenceHandle : StatusHandle(LINKED_STATUS) {
        val closeCalled = CountDownLatch(1)
        val runRejected = CountDownLatch(1)
        val runCalls = AtomicInteger()
        override fun close() {
            super.close()
            closeCalled.countDown()
        }
        override fun run() {
            runCalls.incrementAndGet()
            if (closed) {
                runRejected.countDown()
                throw Exception("link_closed")
            }
        }
    }

    private class BlockingRunHandle : StatusHandle(LINKED_STATUS) {
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        override fun run() {
            runEntered.countDown()
            try {
                allowRunReturn.await()
            } catch (_: InterruptedException) {
                // Cancellation closes the native handle and interrupts the Java wrapper wait.
            }
            if (closed) throw Exception("link_closed")
        }
    }

    private class RetryHandle : StatusHandle(LINKED_STATUS) {
        val runCalls = AtomicInteger()
        override fun run() {
            when (runCalls.incrementAndGet()) {
                1, 2 -> throw Exception("temporary")
                else -> throw Exception("run_refused")
            }
        }
    }

    private class HeldRefreshHandle : StatusHandle(LINKED_STATUS) {
        val runEntered = CountDownLatch(1)
        val allowRunReturn = CountDownLatch(1)
        val refreshStatusEntered = CountDownLatch(1)
        val allowRefreshStatus = CountDownLatch(1)
        val refreshStatusReturned = CountDownLatch(1)
        private val statusCalls = AtomicInteger()
        override fun status(): String {
            if (statusCalls.incrementAndGet() >= 3) {
                refreshStatusEntered.countDown()
                allowRefreshStatus.await()
                refreshStatusReturned.countDown()
            }
            return LINKED_STATUS
        }
        override fun run() {
            runEntered.countDown()
            allowRunReturn.await()
        }
    }

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        val size: Int get() = tasks.size
        override fun execute(command: Runnable) { tasks += command }
    }

    private companion object {
        const val LINKED_STATUS = """{"seq":1,"state":"linked","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val PAIRING_STATUS = """{"seq":1,"state":"pairing","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
        const val UNLINKED_STATUS = """{"seq":1,"state":"unlinked","running":false,"healthy":false,"phone_responding":false,"last_ok_ms":0,"stop_code":""}"""
    }
}
