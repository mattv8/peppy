package dev.peppy.mobile.restrictedsetup

import java.net.InetAddress
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

class LocalEndpointTest {
    private val ipv4 = InetAddress.getByAddress(byteArrayOf(192.toByte(), 0, 2, 10))
    private val ipv6 = InetAddress.getByAddress(byteArrayOf(0x20, 0x01, 0x0d, 0xb8.toByte(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10))
    private val parser = NumericAddressParser { host -> mapOf("192.0.2.10" to ipv4, "2001:db8::10" to ipv6, "fe80::1" to InetAddress.getByAddress(byteArrayOf(0xfe.toByte(), 0x80.toByte(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1)))[host] }
    private val local = setOf(ipv4, ipv6)

    @Test fun `parser accepts current numeric local addresses and derives the socket destination`() {
        val endpoint = LocalEndpoint.parse("192.0.2.10", 37122, local, parser)!!

        assertEquals("192.0.2.10", endpoint.host)
        assertEquals(ipv4, endpoint.socketAddress.address)
        assertEquals(37122, endpoint.socketAddress.port)
    }

    @Test fun `parser rejects names leading zero forms foreign multicast unspecified scoped and invalid ports without invoking parser`() {
        val calls = mutableListOf<String>()
        val recordingParser = NumericAddressParser { calls += it; parser.parse(it) }
        listOf("phone.local", "192.0.2.010", "192.0.2.11", "224.0.0.1", "0.0.0.0", "::", "fe80::1", "fe80::1%wlan0").forEach { host ->
            assertNull(host, LocalEndpoint.parse(host, 37122, local, recordingParser))
        }
        assertNull(LocalEndpoint.parse("192.0.2.10", 0, local, recordingParser))
        assertNull(LocalEndpoint.parse("192.0.2.10", 65536, local, recordingParser))
        assertFalse(calls.contains("phone.local"))
        assertFalse(calls.contains("192.0.2.010"))
        assertFalse(calls.contains("fe80::1%wlan0"))
    }
}
