package dev.peppy.mobile.restrictedsetup

import android.content.Context
import android.annotation.SuppressLint
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import androidx.annotation.RequiresApi
import java.net.InetAddress
import java.util.concurrent.Executor
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch

internal class NsdAdbServiceDiscovery(
    context: Context,
    private val localAddresses: LocalAddressSupplier,
    private val nsd: NsdBrowser = AndroidNsdBrowser(context.getSystemService(NsdManager::class.java)),
    private val apiLevel: Int = Build.VERSION.SDK_INT,
) : AdbServiceDiscovery {
    override fun ports(kind: AdbServiceKind): Flow<Int?> = callbackFlow {
        val serviceType = when (kind) {
            AdbServiceKind.PAIRING -> PAIRING_SERVICE_TYPE
            AdbServiceKind.CONNECT -> CONNECT_SERVICE_TYPE
        }
        val activeCallbacks = mutableListOf<NsdBrowser.ServiceInfoCallback>()
        var trackedOwnServiceName: String? = null
        val listener = object : NsdBrowser.DiscoveryListener {
            override fun onServiceFound(service: NsdBrowser.Service) {
                val callback = object : NsdBrowser.ServiceInfoCallback {
                    override fun onServiceInfo(service: NsdBrowser.Service) {
                        launch {
                            val ownAddress = service.hostAddresses.any { address ->
                                localAddresses.currentAddresses().any { it.address.contentEquals(address.address) }
                            }
                            if (ownAddress) {
                                trackedOwnServiceName = service.name
                                trySend(service.port)
                            }
                        }
                    }

                    override fun onServiceLost() {
                        if (trackedOwnServiceName == service.name) {
                            trackedOwnServiceName = null
                            trySend(null)
                        }
                    }
                }
                activeCallbacks += callback
                if (apiLevel >= API_34) nsd.registerServiceInfoCallback(service, callback) else nsd.resolveService(service, callback)
            }

            override fun onServiceLost(service: NsdBrowser.Service) {
                if (trackedOwnServiceName == service.name) {
                    trackedOwnServiceName = null
                    trySend(null)
                }
            }
        }
        nsd.discoverServices(serviceType, listener)
        awaitClose {
            activeCallbacks.forEach { callback -> runCatching { nsd.unregisterServiceInfoCallback(callback) } }
            runCatching { nsd.stopServiceDiscovery(listener) }
        }
    }.distinctUntilChanged()

    private companion object {
        const val PAIRING_SERVICE_TYPE = "_adb-tls-pairing._tcp"
        const val CONNECT_SERVICE_TYPE = "_adb-tls-connect._tcp"
        const val API_34 = 34
    }
}

internal interface NsdBrowser {
    data class Service(val name: String, val type: String, val hostAddresses: List<InetAddress>, val port: Int)

    interface DiscoveryListener {
        fun onServiceFound(service: Service)
        fun onServiceLost(service: Service)
    }

    interface ServiceInfoCallback {
        fun onServiceInfo(service: Service)
        fun onServiceLost()
    }

    fun discoverServices(serviceType: String, listener: DiscoveryListener)
    fun stopServiceDiscovery(listener: DiscoveryListener)
    fun resolveService(service: Service, callback: ServiceInfoCallback)
    fun registerServiceInfoCallback(service: Service, callback: ServiceInfoCallback)
    fun unregisterServiceInfoCallback(callback: ServiceInfoCallback)
}

private class AndroidNsdBrowser(private val nsd: NsdManager) : NsdBrowser {
    private val listeners = mutableMapOf<NsdBrowser.DiscoveryListener, NsdManager.DiscoveryListener>()
    private val callbacks = mutableMapOf<NsdBrowser.ServiceInfoCallback, NsdManager.ResolveListener>()
    private val infoCallbacks = mutableMapOf<NsdBrowser.ServiceInfoCallback, NsdManager.ServiceInfoCallback>()
    private val directExecutor = Executor { command -> command.run() }
    private val resolveLock = Any()
    private val pendingResolves = ArrayDeque<ResolveRequest>()
    private var resolving = false

    override fun discoverServices(serviceType: String, listener: NsdBrowser.DiscoveryListener) {
        val androidListener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(regType: String) = Unit
            override fun onDiscoveryStopped(serviceType: String) = Unit
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
            override fun onServiceFound(serviceInfo: NsdServiceInfo) = listener.onServiceFound(serviceInfo.toService())
            override fun onServiceLost(serviceInfo: NsdServiceInfo) = listener.onServiceLost(serviceInfo.toService())
        }
        listeners[listener] = androidListener
        nsd.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, androidListener)
    }

    override fun stopServiceDiscovery(listener: NsdBrowser.DiscoveryListener) {
        listeners.remove(listener)?.let(nsd::stopServiceDiscovery)
    }

    override fun resolveService(service: NsdBrowser.Service, callback: NsdBrowser.ServiceInfoCallback) {
        synchronized(resolveLock) {
            pendingResolves += ResolveRequest(service, callback)
            startNextResolve()
        }
    }

    private fun startNextResolve() {
        if (resolving) return
        val request = pendingResolves.removeFirstOrNull() ?: return
        resolving = true
        val listener = object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                synchronized(resolveLock) {
                    resolving = false
                    if (errorCode == NsdManager.FAILURE_ALREADY_ACTIVE && request.attempt < MAX_RESOLVE_RETRIES) {
                        pendingResolves.addFirst(request.copy(attempt = request.attempt + 1))
                    }
                    startNextResolve()
                }
            }

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                request.callback.onServiceInfo(serviceInfo.toService())
                synchronized(resolveLock) {
                    resolving = false
                    startNextResolve()
                }
            }
        }
        callbacks[request.callback] = listener
        nsd.resolveService(NsdServiceInfo().apply { serviceName = request.service.name; serviceType = request.service.type }, listener)
    }

    @SuppressLint("NewApi")
    @RequiresApi(34)
    override fun registerServiceInfoCallback(service: NsdBrowser.Service, callback: NsdBrowser.ServiceInfoCallback) {
        val androidCallback = object : NsdManager.ServiceInfoCallback {
            override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) = Unit
            override fun onServiceInfoCallbackUnregistered() = callback.onServiceLost()
            override fun onServiceUpdated(serviceInfo: NsdServiceInfo) = callback.onServiceInfo(serviceInfo.toService())
            override fun onServiceLost() = callback.onServiceLost()
        }
        infoCallbacks[callback] = androidCallback
        nsd.registerServiceInfoCallback(
            NsdServiceInfo().apply { serviceName = service.name; serviceType = service.type },
            directExecutor,
            androidCallback,
        )
    }

    @SuppressLint("NewApi")
    override fun unregisterServiceInfoCallback(callback: NsdBrowser.ServiceInfoCallback) {
        if (Build.VERSION.SDK_INT >= 34) {
            infoCallbacks.remove(callback)?.let(nsd::unregisterServiceInfoCallback)
        }
        synchronized(resolveLock) {
            pendingResolves.removeAll { it.callback == callback }
            callbacks.remove(callback)
        }
    }

    private fun NsdServiceInfo.toService(): NsdBrowser.Service = NsdBrowser.Service(
        serviceName,
        serviceType,
        host?.let(::listOf).orEmpty(),
        port,
    )

    private data class ResolveRequest(
        val service: NsdBrowser.Service,
        val callback: NsdBrowser.ServiceInfoCallback,
        val attempt: Int = 0,
    )

    private companion object {
        const val MAX_RESOLVE_RETRIES = 3
    }
}
