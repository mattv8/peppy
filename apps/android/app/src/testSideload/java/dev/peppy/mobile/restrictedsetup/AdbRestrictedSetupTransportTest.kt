package dev.peppy.mobile.restrictedsetup

import io.github.muntashirakon.adb.AdbPairingCodeRejectedException
import java.net.InetAddress
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AdbRestrictedSetupTransportTest {
    @Test fun `runs only the fixed commands and closes once`() = runBlocking {
        val manager = FakeManager()
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { manager })
        transport.grantOwnRestrictedAccess()
        transport.turnWirelessDebuggingOff()
        transport.close()
        transport.close()

        assertEquals(
            listOf(
                "appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow",
                "settings put global adb_wifi_enabled 0",
            ),
            manager.commands,
        )
        assertEquals(1, manager.cancelCalls)
        assertEquals(1, manager.closeCalls)
    }

    @Test fun `maps only the library rejected-code signal to rejection`() = runBlocking {
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { FakeManager(rejectPairing = true) })
        val endpoint = LocalEndpoint.parse("127.0.0.1", 37122, setOf(InetAddress.getByName("127.0.0.1")))!!

        val result = runCatching { transport.pair(endpoint, "123456".toCharArray()) }

        assertTrue(result.exceptionOrNull() is PairingRejectedException)
    }

    private class FakeManager(private val rejectPairing: Boolean = false) : AdbRestrictedSetupManager {
        val commands = mutableListOf<String>()
        var cancelCalls = 0
        var closeCalls = 0
        override fun pair(host: String, port: Int, code: ByteArray) {
            if (rejectPairing) throw AdbPairingCodeRejectedException()
        }
        override fun connectTo(host: String, port: Int) = true
        override fun open(command: FixedShellCommand): AdbRestrictedSetupStream {
            commands += command.destination("dev.peppy.mobile").removePrefix("shell:")
            return object : AdbRestrictedSetupStream {
                override fun read(buffer: ByteArray, offset: Int, length: Int) = -1
                override fun close() = Unit
            }
        }
        override fun cancelPairing() { cancelCalls++ }
        override fun close() { closeCalls++ }
    }
}
