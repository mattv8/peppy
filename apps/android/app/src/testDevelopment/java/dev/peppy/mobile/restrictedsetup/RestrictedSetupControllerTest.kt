package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.asCoroutineDispatcher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RestrictedSetupControllerTest {
    @Test fun `preparation stays visible until pairing inputs are requested and cancel returns to introduction`() {
        val controller = controller(CoroutineScope(Dispatchers.Default), MemoryCleanupMarker(), FakeTransport())

        assertTrue(controller.prepare())
        assertEquals(RestrictedSetupStage.PREPARATION, controller.state.value.stage)
        controller.cancel()
        assertEquals(RestrictedSetupStage.INTRODUCTION, controller.state.value.stage)
        assertTrue(controller.prepare())
        assertTrue(controller.awaitPairing())
        assertEquals(RestrictedSetupStage.AWAITING_PAIRING, controller.state.value.stage)
    }

    @Test fun `cleanup is persisted before pairing and survives process death`() = runBlocking {
        val events = mutableListOf<String>()
        val cleanup = MemoryCleanupMarker(events)
        val transport = FakeTransport(events)
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, cleanup, transport)

            begin(controller)
            assertTrue(controller.submit(endpoint(), endpoint(37123), "123456".toCharArray()))
            awaitStage(controller, RestrictedSetupStage.COMPLETE)
            withTimeout(1_000) { while (!transport.closed) delay(1) }

            assertEquals(listOf("cleanup", "pair", "connect", "grant", "close"), events)
            assertTrue(controller(scope, cleanup, FakeTransport()).state.value.cleanupNeeded)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `a second submission cannot start another active attempt`() = runBlocking {
        val transport = FakeTransport().apply { pairBlock = CountDownLatch(1) }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, MemoryCleanupMarker(), transport)
            begin(controller)

            assertTrue(controller.submit(endpoint(), endpoint(37123), "123456".toCharArray()))
            transport.pairStarted.await()
            assertFalse(controller.submit(endpoint(), endpoint(37123), "654321".toCharArray()))
            assertEquals(1, transport.pairCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `watchdog closes a blocking pairing call from an independent dispatcher`() = runBlocking {
        val transport = FakeTransport().apply { pairBlock = CountDownLatch(1) }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope,
                MemoryCleanupMarker(),
                transport,
                deadlines = RestrictedSetupDeadlines(pairingMillis = 20, totalMillis = 500),
            )
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.pairStarted.await()

            awaitStage(controller, RestrictedSetupStage.FAILED)
            assertTrue(transport.closed)
            assertTrue(transport.pairBlockReleased)
            assertEquals(RestrictedSetupFailure.TIMED_OUT, controller.state.value.failure)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `grant timeout closes transport then performs bounded own app readback without replay`() = runBlocking {
        val transport = FakeTransport().apply { grantBlock = CountDownLatch(1) }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope,
                MemoryCleanupMarker(),
                transport,
                readback = { transport.readbackCalls += 1; true },
                deadlines = RestrictedSetupDeadlines(grantAndReadbackMillis = 20, totalMillis = 500),
            )
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.grantStarted.await()

            awaitStage(controller, RestrictedSetupStage.COMPLETE)
            assertTrue(transport.closed)
            assertEquals(1, transport.grantCalls)
            assertEquals(1, transport.readbackCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `slow close never holds the controller lock`() = runBlocking {
        val transport = FakeTransport().apply {
            pairBlock = CountDownLatch(1)
            closeBlock = CountDownLatch(1)
        }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(
                scope,
                MemoryCleanupMarker(),
                transport,
                deadlines = RestrictedSetupDeadlines(pairingMillis = 20, totalMillis = 500),
            )
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.pairStarted.await()
            withTimeout(1_000) { while (!transport.closeEntered) delay(1) }

            val cancellation = async(Dispatchers.Default) { controller.cancel() }
            withTimeout(100) { cancellation.await() }
            assertEquals(RestrictedSetupStage.CLEANUP_REQUIRED, controller.state.value.stage)
            assertFalse(controller.prepare())
            transport.closeBlock?.countDown()
            withTimeout(1_000) { while (!transport.closed) delay(1) }
            assertTrue(controller.prepare())
        } finally {
            transport.closeBlock?.countDown()
            scope.cancel()
        }
    }

    @Test fun `throwing close is contained and blocks future attempts`() = runBlocking {
        val transport = FakeTransport().apply { throwOnClose = true }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, MemoryCleanupMarker(), transport)
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.grantStarted.await()

            withTimeout(1_000) { while (!transport.closeEntered) delay(1) }
            withTimeout(1_000) { while (controller.state.value.stage != RestrictedSetupStage.CLEANUP_REQUIRED) delay(1) }
            assertEquals(RestrictedSetupFailure.TRANSPORT, controller.state.value.failure)
            assertFalse(controller.prepare())
            assertFalse(controller.acknowledgeCleanup())
        } finally {
            scope.cancel()
        }
    }

    @Test fun `transport operations run off the controller dispatcher`() = runBlocking {
        val executor = Executors.newSingleThreadExecutor { Thread(it, "controller-main-test") }
        val dispatcher = executor.asCoroutineDispatcher()
        try {
            val transport = FakeTransport()
            val controller = controller(CoroutineScope(dispatcher), MemoryCleanupMarker(), transport)
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            awaitStage(controller, RestrictedSetupStage.COMPLETE)

            assertFalse(transport.pairThread.startsWith("controller-main-test"))
        } finally {
            dispatcher.close()
            executor.shutdown()
        }
    }

    @Test fun `late cancellation prevents the next transport step`() = runBlocking {
        val transport = FakeTransport().apply { pairBlock = CountDownLatch(1) }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, MemoryCleanupMarker(), transport)
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.pairStarted.await()

            controller.cancel()
            withTimeout(1_000) { while (!transport.closed) delay(1) }
            delay(20)
            assertEquals(0, transport.connectCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `cancel fences an aborted grant exception from stale verification or failure`() = runBlocking {
        val transport = FakeTransport().apply {
            grantBlock = CountDownLatch(1)
            throwGrantAfterClose = true
        }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, MemoryCleanupMarker(), transport)
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.grantStarted.await()

            controller.cancel()
            withTimeout(1_000) { while (!transport.closed) delay(1) }
            delay(20)
            assertEquals(RestrictedSetupStage.CLEANUP_REQUIRED, controller.state.value.stage)
            assertEquals(0, transport.readbackCalls)
            withTimeout(1_000) { while (!controller.prepare()) delay(1) }
        } finally {
            scope.cancel()
        }
    }

    @Test fun `parent cancellation closes resources without publishing a controller transition`() = runBlocking {
        val parent = SupervisorJob()
        val transport = FakeTransport().apply { pairBlock = CountDownLatch(1) }
        val scope = CoroutineScope(parent + Dispatchers.Default)
        val controller = controller(scope, MemoryCleanupMarker(), transport)
        begin(controller)
        controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
        transport.pairStarted.await()

        parent.cancel()
        withTimeout(1_000) { while (!transport.closed) delay(1) }

        assertEquals(RestrictedSetupStage.PAIRING, controller.state.value.stage)
    }

    @Test fun `readback after an unknown grant outcome is bounded`() = runBlocking {
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val transport = FakeTransport()
            val controller = controller(
                scope,
                MemoryCleanupMarker(),
                transport,
                readback = { CompletableDeferred<Boolean>().await() },
                deadlines = RestrictedSetupDeadlines(grantAndReadbackMillis = 20, totalMillis = 500),
            )
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())

            awaitStage(controller, RestrictedSetupStage.FAILED)
            assertEquals(RestrictedSetupFailure.TIMED_OUT, controller.state.value.failure)
            assertEquals(RestrictedSetupAccess.UNKNOWN, controller.state.value.access)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `completion hook clears code and detaches when scope was already cancelled`() = runBlocking {
        val parent = Job().apply { cancel() }
        val controller = controller(CoroutineScope(parent + Dispatchers.Default), MemoryCleanupMarker(), FakeTransport())
        val code = "123456".toCharArray()
        begin(controller)

        assertTrue(controller.submit(endpoint(), endpoint(37123), code))
        withTimeout(1_000) { while (code.any { it != '\u0000' }) delay(1) }
        assertTrue(controller.submit(endpoint(), endpoint(37123), "654321".toCharArray()))
    }

    @Test fun `cleanup acknowledgement is rejected without persisting while an attempt is active`() = runBlocking {
        val marker = MemoryCleanupMarker()
        val transport = FakeTransport().apply { pairBlock = CountDownLatch(1) }
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, marker, transport)
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            transport.pairStarted.await()

            assertFalse(controller.acknowledgeCleanup())
            assertEquals(0, marker.acknowledgements)
        } finally {
            scope.cancel()
        }
    }

    @Test fun `unverified transport failures retain unknown access`() = runBlocking {
        val scope = CoroutineScope(Dispatchers.Default)
        try {
            val controller = controller(scope, MemoryCleanupMarker(), FakeTransport(), localAddresses = { throw IllegalStateException() })
            begin(controller)
            controller.submit(endpoint(), endpoint(37123), "123456".toCharArray())
            awaitStage(controller, RestrictedSetupStage.FAILED)

            assertEquals(RestrictedSetupAccess.UNKNOWN, controller.state.value.access)
        } finally {
            scope.cancel()
        }
    }

    private fun begin(controller: RestrictedSetupController) {
        assertTrue(controller.prepare())
        assertTrue(controller.awaitPairing())
    }

    private fun controller(
        scope: CoroutineScope,
        cleanup: MemoryCleanupMarker,
        transport: FakeTransport,
        readback: suspend () -> Boolean = { transport.readbackCalls += 1; true },
        localAddresses: suspend () -> Set<InetAddress> = { setOf(loopback) },
        deadlines: RestrictedSetupDeadlines = RestrictedSetupDeadlines(),
    ) = RestrictedSetupController(
        controllerScope = scope,
        transportFactory = RestrictedSetupTransportFactory { transport },
        accessReadback = RestrictedSetupAccessReadback(readback),
        localAddresses = LocalAddressSupplier(localAddresses),
        cleanupMarker = cleanup,
        deadlines = deadlines,
    )

    private suspend fun awaitStage(controller: RestrictedSetupController, stage: RestrictedSetupStage) {
        withTimeout(1_000) { while (controller.state.value.stage != stage) delay(1) }
    }

    private fun endpoint(port: Int = 37122) = LocalEndpoint.parse("127.0.0.1", port, setOf(loopback), parser)!!

    private class MemoryCleanupMarker(private val events: MutableList<String> = mutableListOf()) : RestrictedSetupCleanupMarker {
        private var pending = false
        var acknowledgements = 0

        override fun needsCleanup() = pending
        override fun markCleanupNeeded(): Boolean { events += "cleanup"; pending = true; return true }
        override fun acknowledgeCleanup(): Boolean { acknowledgements += 1; pending = false; return true }
    }

    private class FakeTransport(private val events: MutableList<String> = mutableListOf()) : RestrictedSetupTransport {
        val pairStarted = CompletableDeferred<Unit>()
        val grantStarted = CompletableDeferred<Unit>()
        var pairBlock: CountDownLatch? = null
        var grantBlock: CountDownLatch? = null
        var pairCalls = 0
        var connectCalls = 0
        var grantCalls = 0
        var readbackCalls = 0
        var closed = false
        var pairBlockReleased = false
        var closeBlock: CountDownLatch? = null
        var throwOnClose = false
        var throwGrantAfterClose = false
        @Volatile var closeEntered = false
        @Volatile var pairThread = ""

        override suspend fun pair(endpoint: LocalEndpoint, code: CharArray) {
            events += "pair"; pairCalls += 1; pairStarted.complete(Unit)
            pairThread = Thread.currentThread().name
            pairBlock?.await()
            pairBlockReleased = pairBlock != null
        }

        override suspend fun connect(endpoint: LocalEndpoint) { events += "connect"; connectCalls += 1 }
        override suspend fun grantOwnRestrictedAccess() {
            events += "grant"; grantCalls += 1; grantStarted.complete(Unit)
            grantBlock?.await()
            if (throwGrantAfterClose) throw IOException()
        }

        override fun close() {
            closeEntered = true
            closeBlock?.await()
            if (throwOnClose) throw IllegalStateException()
            events += "close"; closed = true; pairBlock?.countDown(); grantBlock?.countDown()
        }
    }

    private companion object {
        val loopback: InetAddress = InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
        val parser = NumericAddressParser { if (it == "127.0.0.1") loopback else null }
    }
}
