package dev.peppy.mobile.restrictedsetup

import io.github.muntashirakon.adb.AdbPairingCodeRejectedException
import java.io.IOException
import java.net.InetAddress
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.assertFalse
import org.junit.Test

class AdbRestrictedSetupTransportTest {
    @Test fun `drains and closes each grant before opening the next without wireless cleanup`() = runBlocking {
        val manager = FakeManager(outputChunks = mapOf(
            FixedShellCommand.GRANT_RESTRICTED to listOf("first chunk", "remaining output"),
            FixedShellCommand.GRANT_BLOCKED_SENDERS to listOf("second grant output"),
        ))
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { manager })
        try {
            transport.grantAdvancedFeatures()

            assertEquals(
                listOf(
                    "appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow",
                    "appops set dev.peppy.mobile WRITE_SMS allow",
                ),
                manager.commands,
            )
            assertEquals(
                listOf(
                    "open:GRANT_RESTRICTED", "read:GRANT_RESTRICTED:first chunk",
                    "read:GRANT_RESTRICTED:remaining output", "eof:GRANT_RESTRICTED", "close:GRANT_RESTRICTED",
                    "open:GRANT_BLOCKED_SENDERS", "read:GRANT_BLOCKED_SENDERS:second grant output",
                    "eof:GRANT_BLOCKED_SENDERS", "close:GRANT_BLOCKED_SENDERS",
                ),
                manager.events,
            )
        } finally { transport.close() }
    }

    @Test fun `real read failure closes first grant and propagates without opening second grant`() = runBlocking {
        val failure = IOException("Connection lost")
        val manager = FakeManager(
            outputChunks = mapOf(FixedShellCommand.GRANT_RESTRICTED to listOf("partial output")),
            readFailures = mapOf(FixedShellCommand.GRANT_RESTRICTED to failure),
        )
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { manager })
        try {
            val result = runCatching { transport.grantAdvancedFeatures() }

            assertSame(failure, result.exceptionOrNull())
            assertEquals(listOf("appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow"), manager.commands)
            assertEquals(
                listOf("open:GRANT_RESTRICTED", "read:GRANT_RESTRICTED:partial output", "error:GRANT_RESTRICTED", "close:GRANT_RESTRICTED"),
                manager.events,
            )

            transport.turnWirelessDebuggingOff()

            assertEquals("settings put global adb_wifi_enabled 0", manager.commands.last())
            assertEquals(listOf("open:WIFI_OFF", "eof:WIFI_OFF", "close:WIFI_OFF"), manager.events.takeLast(3))
            assertEquals(2, manager.commands.size)
        } finally { transport.close() }
    }

    @Test fun `runs only the fixed commands and closes once`() = runBlocking {
        val manager = FakeManager()
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { manager })
        transport.grantAdvancedFeatures()
        transport.turnWirelessDebuggingOff()
        transport.close()
        transport.close()

        assertEquals(
            listOf(
                "appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow",
                "appops set dev.peppy.mobile WRITE_SMS allow",
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

    @Test fun `close failure preserves manager ownership for a controller retry`() {
        val failure = IOException("socket teardown failed")
        val manager = FakeManager(closeFailure = failure)
        val transport = AdbRestrictedSetupTransport("dev.peppy.mobile", managerFactory = { manager })

        val first = runCatching { transport.close() }
        val second = runCatching { transport.close() }

        assertSame(failure, first.exceptionOrNull())
        assertSame(failure, second.exceptionOrNull())
        assertEquals(2, manager.cancelCalls)
        assertEquals(2, manager.closeCalls)
        assertFalse(manager.destroyed)
    }

    private class FakeManager(
        private val rejectPairing: Boolean = false,
        private val outputChunks: Map<FixedShellCommand, List<String>> = emptyMap(),
        private val readFailures: Map<FixedShellCommand, IOException> = emptyMap(),
        private val closeFailure: IOException? = null,
    ) : AdbRestrictedSetupManager {
        val commands = mutableListOf<String>()
        val events = mutableListOf<String>()
        var cancelCalls = 0
        var closeCalls = 0
        var destroyed = false
        override fun pair(host: String, port: Int, code: ByteArray) {
            if (rejectPairing) throw AdbPairingCodeRejectedException()
        }
        override fun connectTo(host: String, port: Int) = true
        override fun open(command: FixedShellCommand): AdbRestrictedSetupStream {
            commands += command.destination("dev.peppy.mobile").removePrefix("shell:")
            events += "open:$command"
            val chunks = outputChunks[command].orEmpty().iterator()
            return object : AdbRestrictedSetupStream {
                override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
                    if (chunks.hasNext()) {
                        val chunk = chunks.next()
                        val bytes = chunk.toByteArray()
                        require(bytes.size <= length)
                        bytes.copyInto(buffer, offset)
                        events += "read:$command:$chunk"
                        return bytes.size
                    }
                    readFailures[command]?.let {
                        events += "error:$command"
                        throw it
                    }
                    events += "eof:$command"
                    return -1
                }
                override fun close() { events += "close:$command" }
            }
        }
        override fun cancelPairing() { cancelCalls++ }
        override fun close() {
            closeCalls++
            closeFailure?.let { throw it }
            destroyed = true
        }
    }
}
