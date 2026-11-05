package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NsdAdbServiceDiscoveryTest {
    @Test fun `uses pairing type filters foreign addresses and unregisters callbacks`() = runBlocking {
        val browser = FakeBrowser()
        val local = InetAddress.getByName("192.0.2.10")
        val discovery = NsdAdbServiceDiscovery(
            context = android.app.Application(),
            localAddresses = LocalAddressSupplier { setOf(local) },
            nsd = browser,
            apiLevel = 34,
        )
        val result = async { discovery.ports(AdbServiceKind.PAIRING).first { it == 37122 } }
        withTimeout(1_000) { while (browser.serviceType == null) yield() }
        assertEquals("_adb-tls-pairing._tcp", browser.serviceType)
        browser.found(NsdBrowser.Service("foreign", "_adb-tls-pairing._tcp", listOf(InetAddress.getByName("192.0.2.11")), 1234))
        browser.found(NsdBrowser.Service("own", "_adb-tls-pairing._tcp", listOf(local), 37122))
        assertEquals(37122, result.await())
        assertTrue(browser.unregistered > 0)
    }

    private class FakeBrowser : NsdBrowser {
        var serviceType: String? = null
        var listener: NsdBrowser.DiscoveryListener? = null
        var callbacks = mutableListOf<NsdBrowser.ServiceInfoCallback>()
        var unregistered = 0
        override fun discoverServices(serviceType: String, listener: NsdBrowser.DiscoveryListener) { this.serviceType = serviceType; this.listener = listener }
        override fun stopServiceDiscovery(listener: NsdBrowser.DiscoveryListener) = Unit
        override fun resolveService(service: NsdBrowser.Service, callback: NsdBrowser.ServiceInfoCallback) { callbacks += callback; callback.onServiceInfo(service) }
        override fun registerServiceInfoCallback(service: NsdBrowser.Service, callback: NsdBrowser.ServiceInfoCallback) { callbacks += callback; callback.onServiceInfo(service) }
        override fun unregisterServiceInfoCallback(callback: NsdBrowser.ServiceInfoCallback) { unregistered++ }
        fun found(service: NsdBrowser.Service) { listener?.onServiceFound(service) }
    }
}
