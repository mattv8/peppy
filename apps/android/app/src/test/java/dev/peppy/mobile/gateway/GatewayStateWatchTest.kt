package dev.peppy.mobile.gateway

import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Delay
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayStateWatchTest {
    @Test
    fun tooLargeRibbonNeedsVaultKeysReady() {
        assertTrue(historyTooLargeRibbonVisible(status(historyTooLarge = true, sharedKeysReady = true)))
        assertFalse("a locked phone is told to unlock, and its Details action has no route", historyTooLargeRibbonVisible(status(historyTooLarge = true, sharedKeysReady = false)))
        assertFalse(historyTooLargeRibbonVisible(status(historyTooLarge = false, sharedKeysReady = true)))
        assertFalse(historyTooLargeRibbonVisible(null))
    }

    @Test
    fun onlyBootstrapPhasesAreImport() {
        assertTrue(SyncPhase.BOOTSTRAP.isBootstrapPhase())
        assertTrue(SyncPhase.BOOTSTRAP_DRAINING.isBootstrapPhase())
        assertFalse(SyncPhase.LIVE.isBootstrapPhase())
        assertFalse(SyncPhase.RECOVERY_REQUIRED.isBootstrapPhase())
    }

    @Test
    fun reloadsAreSpacedOneSecondApart() {
        assertEquals(1_000L, GATEWAY_STATUS_RELOAD_MS)
    }

    @Test
    fun changesDuringThePauseMergeIntoOneReload() = runBlocking {
        val dispatcher = PausedReloadDispatcher()
        val changes = MutableSharedFlow<Unit>(extraBufferCapacity = 16)
        val finishFirstReload = CompletableDeferred<Unit>()
        var reloads = 0
        val watching = launch(dispatcher) {
            coalescedReloads(changes, pauseMs = 300L) {
                reloads++
                if (reloads == 1) finishFirstReload.await()
            }
        }
        fun emitChanges(count: Int) {
            repeat(count) { assertTrue(changes.tryEmit(Unit)) }
            dispatcher.runCurrent()
        }

        try {
            // Drain both the collector launch and conflate's upstream producer before emitting.
            dispatcher.runCurrent()
            assertEquals("the upstream is subscribed", 1, changes.subscriptionCount.value)
            emitChanges(1)
            assertEquals("the initial change reloads once", 1, reloads)

            emitChanges(3)
            assertEquals("changes do not interrupt an active reload", 1, reloads)
            finishFirstReload.complete(Unit)
            dispatcher.runCurrent()
            assertEquals("finishing the reload must still leave a pause", 1, reloads)

            emitChanges(3)
            assertEquals("still inside the pause", 1, reloads)
            dispatcher.finishPause(expectedMs = 300L)
            assertEquals("reload and pause changes merge into one pending reload", 2, reloads)
            dispatcher.finishPause(expectedMs = 300L)
            assertEquals("the burst leaves no extra reload or polling timer", 2, reloads)
            assertTrue(dispatcher.isIdle)

            emitChanges(1)
            assertEquals("a later change starts a new reload", 3, reloads)
            dispatcher.finishPause(expectedMs = 300L)
            assertEquals(3, reloads)
            assertTrue(dispatcher.isIdle)
        } finally {
            watching.cancel()
            dispatcher.runCurrent()
        }
    }

    /** Runs queued coroutine work explicitly; delay stays suspended until the test releases it. */
    @OptIn(InternalCoroutinesApi::class)
    private class PausedReloadDispatcher : CoroutineDispatcher(), Delay {
        private val work = ArrayDeque<Runnable>()
        private val pauses = ArrayDeque<Pair<Long, CancellableContinuation<Unit>>>()
        val isIdle: Boolean get() = work.isEmpty() && pauses.isEmpty()

        override fun dispatch(context: CoroutineContext, block: Runnable) {
            work.addLast(block)
        }

        override fun scheduleResumeAfterDelay(timeMillis: Long, continuation: CancellableContinuation<Unit>) {
            pauses.addLast(timeMillis to continuation)
        }

        fun runCurrent() {
            while (work.isNotEmpty()) work.removeFirst().run()
        }

        fun finishPause(expectedMs: Long) {
            assertEquals("exactly one reload pause is suspended", 1, pauses.size)
            val (durationMs, continuation) = pauses.removeFirst()
            assertEquals("the requested pause separates reloads", expectedMs, durationMs)
            continuation.resume(Unit)
            runCurrent()
        }
    }

    private fun status(historyTooLarge: Boolean, sharedKeysReady: Boolean) = GatewayStatus(
        enrolled = true,
        origin = null,
        databaseOpen = sharedKeysReady,
        sharedKeysReady = sharedKeysReady,
        syncPhase = SyncPhase.LIVE,
        historyTooLarge = historyTooLarge,
    )
}
