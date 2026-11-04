package dev.peppy.mobile.restrictedsetup

import android.os.Build
import io.github.muntashirakon.adb.AdbPairingCodeRejectedException
import io.github.muntashirakon.adb.AbsAdbConnectionManager
import io.github.muntashirakon.adb.PairingConnectionCtx
import java.nio.CharBuffer
import java.nio.charset.StandardCharsets
import java.io.IOException
import java.security.PrivateKey
import java.security.cert.Certificate
import java.util.concurrent.TimeUnit

internal class AdbRestrictedSetupTransport(
    packageName: String,
    private val identityFactory: () -> EphemeralAdbIdentity = EphemeralAdbIdentity::create,
    private val managerFactory: (EphemeralAdbIdentity) -> AdbRestrictedSetupManager = { AdbConnectionManager(it, packageName) },
) : RestrictedSetupTransport {
    private val packageName = packageName.requireValidPackageName()
    @Volatile private var identity: EphemeralAdbIdentity? = identityFactory()
    @Volatile private var manager: AdbRestrictedSetupManager? = managerFactory(checkNotNull(identity))
    @Volatile private var closed = false

    override suspend fun pair(endpoint: LocalEndpoint, code: CharArray) {
        checkOpen()
        val bytes = code.toUtf8Bytes()
        try {
            manager().pair(LOOPBACK_HOST, endpoint.port, bytes)
        } catch (error: AdbPairingCodeRejectedException) {
            throw PairingRejectedException()
        } finally {
            bytes.fill(0)
        }
    }

    override suspend fun connect(endpoint: LocalEndpoint) {
        checkOpen()
        check(manager().connectTo(LOOPBACK_HOST, endpoint.port)) { "Connection failed" }
    }

    override suspend fun grantOwnRestrictedAccess() {
        runFixedShellCommand(FixedShellCommand.GRANT_RESTRICTED, tolerateEofAfterWrite = false)
    }

    override suspend fun turnWirelessDebuggingOff() {
        runFixedShellCommand(FixedShellCommand.WIFI_OFF, tolerateEofAfterWrite = true)
    }

    override fun close() = synchronized(this) {
        if (closed) return
        closed = true
        manager?.cancelPairing()
        runCatching { manager?.close() }
        manager = null
        identity?.destroy()
        identity = null
    }

    private fun runFixedShellCommand(command: FixedShellCommand, tolerateEofAfterWrite: Boolean) {
        checkOpen()
        val stream = manager().open(command)
        try {
            drain(stream)
        } catch (error: IOException) {
            if (!tolerateEofAfterWrite) throw error
        } finally {
            runCatching { stream.close() }
        }
    }

    private fun drain(stream: AdbRestrictedSetupStream) {
        val buffer = ByteArray(DRAIN_BUFFER_SIZE)
        while (stream.read(buffer, 0, buffer.size) >= 0) Unit
    }

    private fun manager(): AdbRestrictedSetupManager = checkNotNull(manager) { "Transport is closed" }

    private fun checkOpen() = check(!closed) { "Transport is closed" }

    private class AdbConnectionManager(
        private val identity: EphemeralAdbIdentity,
        private val packageName: String,
    ) : AbsAdbConnectionManager(), AdbRestrictedSetupManager {
        private val pairingLock = Any()
        @Volatile private var livePairing: PairingConnectionCtx? = null
        @Volatile private var pairingCancelled = false
        init {
            setApi(Build.VERSION.SDK_INT)
            setTimeout(CONNECTION_TIMEOUT_MILLIS, TimeUnit.MILLISECONDS)
        }

        override fun getPrivateKey(): PrivateKey = identity.privateKey
        override fun getCertificate(): Certificate = identity.certificate
        override fun getDeviceName(): String = EphemeralAdbIdentity.DEVICE_NAME
        override fun pair(host: String, port: Int, code: ByteArray) {
            val pairing = PairingConnectionCtx(
                host,
                port,
                code,
                identity.privateKey,
                identity.certificate,
                EphemeralAdbIdentity.DEVICE_NAME,
                CONNECTION_TIMEOUT_MILLIS.toInt(),
            )
            try {
                synchronized(pairingLock) {
                    livePairing = pairing
                    if (pairingCancelled) pairing.close()
                }
                pairing.start()
            } finally {
                code.fill(0)
                synchronized(pairingLock) {
                    if (livePairing === pairing) livePairing = null
                }
                pairing.close()
            }
        }
        override fun cancelPairing() {
            super.cancelPairing()
            val pairing = synchronized(pairingLock) {
                pairingCancelled = true
                livePairing
            }
            pairing?.close()
        }
        override fun connectTo(host: String, port: Int): Boolean = super.connect(host, port)
        override fun open(command: FixedShellCommand): AdbRestrictedSetupStream = AdbStreamAdapter(openStream(command.destination(packageName)))
    }

    private companion object {
        const val LOOPBACK_HOST = "127.0.0.1"
        const val CONNECTION_TIMEOUT_MILLIS = 30_000L
        const val DRAIN_BUFFER_SIZE = 4096
    }
}

internal interface AdbRestrictedSetupManager : AutoCloseable {
    fun pair(host: String, port: Int, code: ByteArray)
    fun connectTo(host: String, port: Int): Boolean
    fun open(command: FixedShellCommand): AdbRestrictedSetupStream
    fun cancelPairing()
}

internal enum class FixedShellCommand {
    GRANT_RESTRICTED,
    WIFI_OFF;

    fun destination(packageName: String): String = when (this) {
        GRANT_RESTRICTED -> "shell:appops set $packageName READ_RESTRICTED_MESSAGES allow"
        WIFI_OFF -> "shell:settings put global adb_wifi_enabled 0"
    }
}

internal interface AdbRestrictedSetupStream : AutoCloseable {
    fun read(buffer: ByteArray, offset: Int, length: Int): Int
}

private class AdbStreamAdapter(private val stream: io.github.muntashirakon.adb.AdbStream) : AdbRestrictedSetupStream {
    override fun read(buffer: ByteArray, offset: Int, length: Int): Int = stream.read(buffer, offset, length)
    override fun close() = stream.close()
}

private fun String.requireValidPackageName(): String = also {
    require(Regex("[A-Za-z0-9._]+").matches(it)) { "Invalid package name" }
}

private fun CharArray.toUtf8Bytes(): ByteArray {
    val workspace = ByteArray(size * 3)
    val output = java.nio.ByteBuffer.wrap(workspace)
    try {
        val encoder = StandardCharsets.UTF_8.newEncoder()
        check(!encoder.encode(CharBuffer.wrap(this), output, true).isError)
        check(!encoder.flush(output).isError)
        return workspace.copyOf(output.position())
    } finally {
        workspace.fill(0)
    }
}
