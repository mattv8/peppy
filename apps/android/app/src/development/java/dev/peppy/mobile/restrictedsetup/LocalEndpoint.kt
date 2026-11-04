package dev.peppy.mobile.restrictedsetup

import android.net.InetAddresses
import android.os.Build
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress

internal fun interface NumericAddressParser {
    fun parse(address: String): InetAddress?
}

internal object AndroidNumericAddressParser : NumericAddressParser {
    override fun parse(address: String): InetAddress? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) runCatching { InetAddresses.parseNumericAddress(address) }.getOrNull() else null
}

internal class LocalEndpoint private constructor(private val address: InetAddress, val port: Int) {
    val host: String = address.hostAddress
    val socketAddress: InetSocketAddress = InetSocketAddress(address, port)

    internal fun isCurrentLocalAddress(localAddresses: Set<InetAddress>): Boolean =
        !address.isAnyLocalAddress &&
            !address.isMulticastAddress &&
            !address.isLinkLocalAddress &&
            (address.isLoopbackAddress || localAddresses.any { it.address.contentEquals(address.address) })

    internal companion object {
        fun parse(
            host: String,
            port: Int,
            localAddresses: Set<InetAddress>,
            parser: NumericAddressParser = AndroidNumericAddressParser,
        ): LocalEndpoint? {
            if (port !in 1..65535 || !isUnambiguousNumericAddress(host)) return null
            val address = parser.parse(host) ?: return null
            if (address is Inet6Address && address.isLinkLocalAddress) return null
            return LocalEndpoint(address, port).takeIf { it.isCurrentLocalAddress(localAddresses) }
        }

        private fun isUnambiguousNumericAddress(host: String): Boolean {
            if ('%' in host) return false
            if (':' in host) return host.matches(Regex("[0-9A-Fa-f:.]+"))
            val octets = host.split('.')
            return octets.size == 4 && octets.all { octet ->
                octet.matches(Regex("[0-9]{1,3}")) &&
                    (octet.length == 1 || octet.first() != '0') &&
                    octet.toInt() in 0..255
            }
        }
    }
}

internal fun interface LocalAddressSupplier {
    suspend fun currentAddresses(): Set<InetAddress>
}
