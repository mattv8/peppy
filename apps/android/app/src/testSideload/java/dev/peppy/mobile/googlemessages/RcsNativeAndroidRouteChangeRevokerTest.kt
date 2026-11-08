package dev.peppy.mobile.googlemessages

import android.content.Context
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.telephony.SubscriptionManager
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.telephony.SimRoute
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [26])
class RcsNativeAndroidRouteChangeRevokerTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Test
    fun `legacy registration from a worker uses the revoker thread and close removes it`() {
        val registrar = RecordingRegistrar()
        val currentRoute = AtomicReference(BOUND_SIM)
        val workerThread = AtomicReference<HandlerThread>()
        val revocations = AtomicInteger()
        val revoker = revoker(registrar, currentRoute, workerThread)

        val registration = AtomicReference<AutoCloseable>()
        val caller = thread(name = "plain-registration-worker") {
            assertNull(Looper.myLooper())
            registration.set(revoker.register(BOUND_ROUTE, revocations::incrementAndGet))
        }
        caller.join(1_000)

        assertFalse("registration caller did not finish", caller.isAlive)
        assertSame(workerThread.get(), registrar.legacyRegistrationThread)
        currentRoute.set(CHANGED_SIM)
        registrar.fireLegacyChange()
        assertTrue(registrar.callbackComplete.await(1, TimeUnit.SECONDS))
        assertSame(workerThread.get(), registrar.callbackThread)
        assertEquals(1, revocations.get())

        registration.get().close()
        assertTrue(registrar.removed.await(1, TimeUnit.SECONDS))
        awaitStopped(workerThread.get())
        assertNull(registrar.registeredListener())
    }

    @Test
    fun `legacy registration failure reaches the caller and stops its thread`() {
        val expected = IllegalStateException("synthetic add failure")
        val registrar = RecordingRegistrar(beforeLegacyAdd = { throw expected })
        val workerThread = AtomicReference<HandlerThread>()
        val revoker = revoker(registrar, AtomicReference(BOUND_SIM), workerThread)

        val failure = assertThrows(IllegalStateException::class.java) {
            revoker.register(BOUND_ROUTE) { error("must not revoke") }
        }

        assertSame(expected, failure.cause)
        assertEquals(1, registrar.legacyAddCalls.get())
        awaitStopped(workerThread.get())
        assertNull(registrar.registeredListener())
    }

    @Test
    fun `legacy registration completing after timeout is removed and stops its thread`() {
        val addEntered = CountDownLatch(1)
        val releaseAdd = CountDownLatch(1)
        val registrar = RecordingRegistrar(beforeLegacyAdd = {
            addEntered.countDown()
            assertTrue(releaseAdd.await(1, TimeUnit.SECONDS))
        })
        val workerThread = AtomicReference<HandlerThread>()
        val revoker = revoker(
            registrar = registrar,
            currentRoute = AtomicReference(BOUND_SIM),
            workerThread = workerThread,
            waitForRegistration = {
                assertTrue(addEntered.await(1, TimeUnit.SECONDS))
                false
            },
        )

        val failure = assertThrows(IllegalStateException::class.java) {
            revoker.register(BOUND_ROUTE) { error("late listener must not revoke") }
        }
        assertEquals("route listener registration timed out", failure.message)

        releaseAdd.countDown()
        assertTrue(registrar.removed.await(1, TimeUnit.SECONDS))
        awaitStopped(workerThread.get())
        assertNull(registrar.registeredListener())
    }

    @Test
    @Config(sdk = [30])
    fun `api 30 registers with an executor and unregisters`() {
        val registrar = RecordingRegistrar()
        val revocations = AtomicInteger()
        val currentRoute = AtomicReference(BOUND_SIM)
        val workerCreations = AtomicInteger()
        val revokerThread = AtomicReference<HandlerThread>()
        val revoker = RcsNativeAndroidRouteChangeRevoker(
            context = context,
            registrar = registrar,
            currentRoute = currentRoute::get,
            workerFactory = {
                workerCreations.incrementAndGet()
                HandlerThread("test-api-30-route-revoker").also(revokerThread::set)
            },
        )

        val registration = AtomicReference<AutoCloseable>()
        val caller = thread(name = "plain-api-30-registration-worker") {
            assertNull(Looper.myLooper())
            registration.set(revoker.register(BOUND_ROUTE, revocations::incrementAndGet))
        }
        caller.join(1_000)

        assertFalse("API 30 registration caller did not finish", caller.isAlive)
        val registered = checkNotNull(registration.get())
        try {
            assertEquals(1, workerCreations.get())
            assertTrue(registrar.executorRegistered)

            currentRoute.set(CHANGED_SIM)
            registrar.fireExecutorChange()
            assertEquals(1, revocations.get())
        } finally {
            registered.close()
        }
        assertTrue(registrar.removed.await(1, TimeUnit.SECONDS))
        awaitStopped(checkNotNull(revokerThread.get()))
        assertNull(registrar.registeredListener())
    }

    private fun revoker(
        registrar: RecordingRegistrar,
        currentRoute: AtomicReference<SimRoute?>,
        workerThread: AtomicReference<HandlerThread>,
        waitForRegistration: (CountDownLatch) -> Boolean = { it.await(1, TimeUnit.SECONDS) },
    ) = RcsNativeAndroidRouteChangeRevoker(
        context = context,
        registrar = registrar,
        currentRoute = currentRoute::get,
        workerFactory = {
            HandlerThread("test-rcs-route-revoker").also(workerThread::set)
        },
        waitForRegistration = waitForRegistration,
    )

    private fun awaitStopped(worker: HandlerThread) {
        worker.join(1_000)
        assertFalse("revoker HandlerThread remained alive", worker.isAlive)
    }

    private class RecordingRegistrar(
        private val beforeLegacyAdd: () -> Unit = {},
    ) : RcsNativeSubscriptionChangeRegistrar {
        val legacyAddCalls = AtomicInteger()
        val callbackComplete = CountDownLatch(1)
        val removed = CountDownLatch(1)
        @Volatile var legacyRegistrationThread: Thread? = null
        @Volatile var callbackThread: Thread? = null
        @Volatile var executorRegistered = false
        private var legacyHandler: Handler? = null
        private var executor: Executor? = null
        private var listener: SubscriptionManager.OnSubscriptionsChangedListener? = null

        override fun addLegacy(listener: SubscriptionManager.OnSubscriptionsChangedListener) {
            legacyAddCalls.incrementAndGet()
            legacyRegistrationThread = Thread.currentThread()
            legacyHandler = Handler(checkNotNull(Looper.myLooper()))
            beforeLegacyAdd()
            this.listener = listener
        }

        override fun addWithExecutor(executor: Executor, listener: SubscriptionManager.OnSubscriptionsChangedListener) {
            executorRegistered = true
            this.executor = executor
            this.listener = listener
        }

        override fun remove(listener: SubscriptionManager.OnSubscriptionsChangedListener) {
            if (this.listener === listener) this.listener = null
            removed.countDown()
        }

        fun fireLegacyChange() {
            checkNotNull(legacyHandler).post {
                callbackThread = Thread.currentThread()
                checkNotNull(listener).onSubscriptionsChanged()
                callbackComplete.countDown()
            }
        }

        fun fireExecutorChange() {
            checkNotNull(executor).execute { checkNotNull(listener).onSubscriptionsChanged() }
        }

        fun registeredListener(): SubscriptionManager.OnSubscriptionsChangedListener? = listener
    }

    private companion object {
        val BOUND_SIM = SimRoute("android-subscription-7", 7, "Default SMS SIM")
        val CHANGED_SIM = SimRoute("android-subscription-8", 8, "Default SMS SIM")
        val BOUND_ROUTE = RcsNativeRouteEvidence(
            routeId = BOUND_SIM.routeId,
            subscriptionId = BOUND_SIM.subscriptionId,
            ownNumberRaw = "+15555550100",
            ownNumberSource = RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber,
            identityRegion = "US",
            observedAtMs = 1,
        )
    }
}
