package dev.peppy.mobile.googlemessages

import android.content.Context
import android.os.Looper
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkCoreBridgeTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @After fun cleanUp() {
        LinkRuntime.resetForTest()
        LinkCoreBridge.resetForTest()
    }


    @Test
    fun `main thread owner unlink fence does not wait for blocked lifecycle publication`() {
        val recorderEntered = CountDownLatch(1)
        val releaseRecorder = CountDownLatch(1)
        val fenceReturned = CountDownLatch(1)
        val recorderDone = CountDownLatch(1)
        var returnedBeforeRelease = false
        LinkCoreBridge.recorder = { _, _ ->
            recorderEntered.countDown()
            releaseRecorder.await()
            true
        }
        Thread {
            LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 1, ""))
            recorderDone.countDown()
        }.start()
        assertTrue(recorderEntered.await(1, TimeUnit.SECONDS))
        val watchdog = Thread {
            returnedBeforeRelease = fenceReturned.await(500, TimeUnit.MILLISECONDS)
            releaseRecorder.countDown()
        }.also { it.start() }

        assertEquals(Looper.getMainLooper(), Looper.myLooper())
        LinkCoreBridge.invalidateObservations()
        fenceReturned.countDown()

        watchdog.join(1_000)
        assertTrue(returnedBeforeRelease)
        assertTrue(recorderDone.await(1, TimeUnit.SECONDS))
    }

    @Test
    fun `stale unlinked observation is dropped after pre-start fence begins`() {
        val validationEntered = CountDownLatch(1)
        val allowValidation = CountDownLatch(1)
        val fenceBegun = CountDownLatch(1)
        val observationDone = CountDownLatch(1)
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }
        LinkCoreBridgeTestHooks.beforeObservationValidation = {
            validationEntered.countDown()
            allowValidation.await()
        }
        LinkCoreBridgeTestHooks.preStartFenceBegun = { fenceBegun.countDown() }

        assertFalse(LinkRuntime.pairingInProgress.get())
        val lease = observationLease()
        Thread {
            LinkCoreBridge.recordObservation(LinkCoreBridge.observationToken(), lease, context, LinkStatus(0, "unlinked", false, false, false, 0, ""))
            observationDone.countDown()
        }.start()
        assertTrue(validationEntered.await(1, TimeUnit.SECONDS))
        val pairing = LinkRuntime.tryBeginPairing()!!
        val fence = Thread { LinkCoreBridge.recordPreStart(context) }.also { it.start() }
        assertTrue(fenceBegun.await(1, TimeUnit.SECONDS))
        allowValidation.countDown()
        assertTrue(observationDone.await(1, TimeUnit.SECONDS))
        fence.join(1_000)

        assertEquals(listOf("unknown"), recorded)
        assertTrue(LinkRuntime.endPairing(pairing))
    }

    @Test
    fun `unlinked observation read before a completed pairing fence is dropped`() {
        val recorded = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> synchronized(recorded) { recorded += status.state }; true }

        val token = LinkCoreBridge.observationToken()
        val lease = observationLease()
        val pairing = LinkRuntime.tryBeginPairing()!!
        assertTrue(LinkCoreBridge.recordPreStart(context))
        assertTrue(LinkRuntime.endPairing(pairing))

        assertTrue(LinkCoreBridge.recordObservation(token, lease, context, LinkStatus(0, "unlinked", false, false, false, 0, "")))

        assertEquals(listOf("unknown"), recorded)
    }

    @Test
    fun `owner completion publishes unlinked before completing core`() {
        val events = mutableListOf<String>()
        LinkCoreBridge.recorder = { _, status -> events += status.state; true }
        LinkCoreBridge.explicitUnlinkCompleter = { _, _ -> events += "completed" }

        assertTrue(LinkCoreBridge.recordExplicitUnlinkCompleted(context))

        assertEquals(listOf("unlinked", "completed"), events)
    }

    @Test
    fun `cutover acknowledgement is pruned so a later incident is shown again`() {
        val alerts = mutableSetOf("conversation")
        LinkCoreBridgeTestHooks.cutoverAlertIds = { alerts.toSet() }

        assertTrue(LinkCoreBridge.cutoverMayHaveMissedMessages(context))
        LinkCoreBridge.clearCutoverMayHaveMissedMessages(context)
        assertFalse(LinkCoreBridge.cutoverMayHaveMissedMessages(context))

        alerts.clear()
        assertFalse(LinkCoreBridge.cutoverMayHaveMissedMessages(context))
        alerts += "conversation"
        assertTrue(LinkCoreBridge.cutoverMayHaveMissedMessages(context))
    }

    @Test
    fun `explicit completion failure does not retry`() {
        var attempts = 0
        LinkCoreBridge.recorder = { _, _ -> true }
        LinkCoreBridge.explicitUnlinkCompleter = { _, _ -> attempts++; throw IllegalStateException() }

        assertFalse(LinkCoreBridge.recordExplicitUnlinkCompleted(context))

        assertEquals(1, attempts)
    }

    @Test
    fun `linked transition enqueues one gateway pass but a linked heartbeat does not`() {
        var enqueues = 0
        LinkCoreBridge.recorder = { _, _ -> true }
        LinkCoreBridgeTestHooks.enqueueGatewayPass = { assertFalse(Thread.holdsLock(LinkCoreBridge)); enqueues += 1 }

        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "unlinked", false, false, false, 0, "")))
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 0, "")))
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 0, "")))

        assertEquals(1, enqueues)
    }

    @Test
    fun `changed linked facts are recorded without another gateway pass`() {
        var enqueues = 0
        val recorded = mutableListOf<LinkStatus>()
        LinkCoreBridge.recorder = { _, status -> recorded += status; true }
        LinkCoreBridgeTestHooks.enqueueGatewayPass = { assertFalse(Thread.holdsLock(LinkCoreBridge)); enqueues += 1 }

        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "unlinked", false, false, false, 0, "")))
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 0, "")))
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, false, true, 0, "")))

        assertEquals(3, recorded.size)
        assertEquals(1, enqueues)
    }

    @Test
    fun `failed lifecycle record and failed enqueue do not fail linking`() {
        LinkCoreBridge.recorder = { _, _ -> false }
        assertFalse(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 0, "")))

        LinkCoreBridge.recorder = { _, _ -> true }
        var enqueueAttempts = 0
        LinkCoreBridgeTestHooks.enqueueGatewayPass = { enqueueAttempts += 1; assertFalse(Thread.holdsLock(LinkCoreBridge)); throw IllegalStateException() }
        assertTrue(LinkCoreBridge.recordIfNeeded(context, LinkStatus(0, "linked", true, true, true, 0, "")))
        assertEquals(1, enqueueAttempts)
    }

    @Test
    fun `linked observation transition enqueues one gateway pass outside the monitor`() {
        var enqueues = 0
        LinkCoreBridge.recorder = { _, _ -> true }
        LinkCoreBridgeTestHooks.enqueueGatewayPass = { assertFalse(Thread.holdsLock(LinkCoreBridge)); enqueues += 1 }
        val lease = observationLease()

        assertTrue(LinkCoreBridge.recordObservation(LinkCoreBridge.observationToken(), lease, context, LinkStatus(0, "linked", true, true, true, 0, "")))

        assertEquals(1, enqueues)
    }

    private fun observationLease(): LinkHandleLease {
        LinkRuntimeTestHooks.engine = object : LinkEngine {
            override val available = true
            override fun open(state: ByteArray, host: LinkHost) = object : LinkHandle {
                override fun pair(cookiesJson: ByteArray) = Unit
                override fun relink() = Unit
                override fun run() = Unit
                override fun checkHealth() = 0L
                override fun unlink() = Unit
                override fun stop() = Unit
                override fun close() = Unit
                override fun status() = "{}"
            }
        }
        LinkRuntimeTestHooks.storeFactory = { object : LinkStateStorage {
            override fun read() = ByteArray(0)
            override fun write(state: ByteArray) = Unit
            override fun wipe() = Unit
        } }
        return LinkRuntime.handleLease(context)!!
    }
}
