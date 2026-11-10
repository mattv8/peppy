package dev.peppy.mobile.googlemessages

import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.telephony.SubscriptionManager
import androidx.annotation.RequiresApi
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

internal interface RcsNativeSubscriptionChangeRegistrar {
    fun addLegacy(listener: SubscriptionManager.OnSubscriptionsChangedListener)
    @RequiresApi(Build.VERSION_CODES.R)
    fun addWithExecutor(executor: Executor, listener: SubscriptionManager.OnSubscriptionsChangedListener)
    fun remove(listener: SubscriptionManager.OnSubscriptionsChangedListener)
}

private class AndroidSubscriptionChangeRegistrar(
    private val subscriptions: SubscriptionManager,
) : RcsNativeSubscriptionChangeRegistrar {
    override fun addLegacy(listener: SubscriptionManager.OnSubscriptionsChangedListener) {
        subscriptions.addOnSubscriptionsChangedListener(listener)
    }

    @RequiresApi(Build.VERSION_CODES.R)
    override fun addWithExecutor(executor: Executor, listener: SubscriptionManager.OnSubscriptionsChangedListener) {
        subscriptions.addOnSubscriptionsChangedListener(executor, listener)
    }

    override fun remove(listener: SubscriptionManager.OnSubscriptionsChangedListener) {
        subscriptions.removeOnSubscriptionsChangedListener(listener)
    }
}

/** Observes default-SMS route changes and directly revokes only the ticket it registered. */
internal class RcsNativeAndroidRouteChangeRevoker(
    context: Context,
    private val registrar: RcsNativeSubscriptionChangeRegistrar = AndroidSubscriptionChangeRegistrar(
        context.getSystemService(SubscriptionManager::class.java),
    ),
    private val currentRoute: () -> SimRoute? = { SimRoutes.current().singleOrNull() },
    private val workerFactory: () -> HandlerThread = { HandlerThread("rcs-route-revoker") },
    private val waitForRegistration: (CountDownLatch) -> Boolean = {
        it.await(REGISTRATION_TIMEOUT_MS, TimeUnit.MILLISECONDS)
    },
) : RcsNativeRouteChangeRevoker {
    override fun register(boundRoute: RcsNativeRouteEvidence, revokeTicket: () -> Unit): AutoCloseable =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) registerWithExecutor(boundRoute, revokeTicket)
        else registerOnLooper(boundRoute, revokeTicket)

    @RequiresApi(Build.VERSION_CODES.R)
    private fun registerWithExecutor(boundRoute: RcsNativeRouteEvidence, revokeTicket: () -> Unit): AutoCloseable =
        registerOnLooper(boundRoute, revokeTicket) { listener ->
            registrar.addWithExecutor(Executor { it.run() }, listener)
        }

    private fun registerOnLooper(
        boundRoute: RcsNativeRouteEvidence,
        revokeTicket: () -> Unit,
        install: (SubscriptionManager.OnSubscriptionsChangedListener) -> Unit = registrar::addLegacy,
    ): AutoCloseable {
        val thread = workerFactory().also { it.start() }
        val handler = Handler(thread.looper)
        val closed = AtomicBoolean(false)
        val completion = CountDownLatch(1)
        val installed = AtomicReference<SubscriptionManager.OnSubscriptionsChangedListener?>()
        val failure = AtomicReference<Throwable?>()
        fun cleanup() {
            if (!closed.compareAndSet(false, true)) return
            val removeAndQuit: () -> Unit = {
                installed.getAndSet(null)?.let { runCatching { registrar.remove(it) } }
                thread.quitSafely()
            }
            if (!handler.post(removeAndQuit)) thread.quitSafely()
        }
        if (!handler.post {
                try {
                    val listener = listener(boundRoute, revokeTicket)
                    install(listener)
                    installed.set(listener)
                    if (closed.get()) {
                        registrar.remove(listener)
                        installed.compareAndSet(listener, null)
                        thread.quitSafely()
                    }
                } catch (error: Throwable) {
                    failure.set(error)
                    thread.quitSafely()
                } finally {
                    completion.countDown()
                }
            }) {
            thread.quitSafely()
            throw IllegalStateException("route listener registration could not start")
        }
        try {
            if (!waitForRegistration(completion)) {
                cleanup()
                throw IllegalStateException("route listener registration timed out")
            }
        } catch (interrupted: InterruptedException) {
            cleanup()
            Thread.currentThread().interrupt()
            throw interrupted
        }
        failure.get()?.let {
            cleanup()
            throw IllegalStateException("route listener registration failed", it)
        }
        return AutoCloseable { cleanup() }
    }

    private fun listener(
        boundRoute: RcsNativeRouteEvidence,
        revokeTicket: () -> Unit,
    ): SubscriptionManager.OnSubscriptionsChangedListener {
        val revoked = AtomicBoolean(false)
        return object : SubscriptionManager.OnSubscriptionsChangedListener() {
            override fun onSubscriptionsChanged() = revokeIfRouteChanged(boundRoute, revoked, revokeTicket)
        }
    }

    private fun revokeIfRouteChanged(
        boundRoute: RcsNativeRouteEvidence,
        revoked: AtomicBoolean,
        revokeTicket: () -> Unit,
    ) {
        val current = currentRoute()
        if ((current?.routeId != boundRoute.routeId || current.subscriptionId != boundRoute.subscriptionId) &&
            revoked.compareAndSet(false, true)
        ) {
            revokeTicket()
        }
    }

    private companion object {
        const val REGISTRATION_TIMEOUT_MS = 5_000L
    }
}
