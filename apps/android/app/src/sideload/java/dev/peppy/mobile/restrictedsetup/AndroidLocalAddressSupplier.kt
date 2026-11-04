package dev.peppy.mobile.restrictedsetup

import android.content.Context
import android.net.ConnectivityManager
import java.net.InetAddress

internal class AndroidLocalAddressSupplier(context: Context) : LocalAddressSupplier {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)

    override suspend fun currentAddresses(): Set<InetAddress> = buildSet {
        add(InetAddress.getLoopbackAddress())
        connectivity.allNetworks.forEach { network ->
            connectivity.getLinkProperties(network)?.linkAddresses?.forEach { add(it.address) }
        }
    }
}
