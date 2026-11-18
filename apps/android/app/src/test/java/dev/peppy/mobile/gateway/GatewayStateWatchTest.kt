package dev.peppy.mobile.gateway

import kotlinx.coroutines.delay
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
        val changes = MutableSharedFlow<Unit>(extraBufferCapacity = 16)
        var reloads = 0
        val watching = launch { coalescedReloads(changes, pauseMs = 300L) { reloads++ } }
        delay(20)

        changes.emit(Unit)
        delay(50)
        assertEquals(1, reloads)

        repeat(3) { changes.emit(Unit) }
        delay(50)
        assertEquals("still inside the pause", 1, reloads)

        delay(450)
        assertEquals("the burst is one pending reload", 2, reloads)
        watching.cancel()
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
