package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.nio.file.Files
import java.util.concurrent.CancellationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RcsMediaPrivateFilesTest {
    @Test fun `actual native decrypted file is admitted and removed`() {
        val files = privateFiles()
        val native = dev.peppy.mobile.gateway.SharedVault.desktop
        val source = files.createFile().also { it.writeText("binding plaintext") }
        val attachment = native.prepareAttachment(source.absolutePath, "text/plain", "native.txt")
        val exported = java.io.File(uniffi.peppy_mobile_bindings.gatewayDecryptRcsAttachmentToFile(native, attachment.attachmentId, files.rootForTest().absolutePath))
        assertEquals(exported.canonicalFile, files.safePath(exported))
        assertEquals("binding plaintext", exported.readText())
        files.delete(exported)
        files.delete(source)
        assertFalse(exported.exists())
    }
    @Test fun `Rust bin exports and interrupted Go downloads are owned and swept`() {
        val files = privateFiles()
        val rust = files.rootForTest().resolve("12345678-1234-1234-1234-123456789abc.bin").also { it.writeText("plain") }
        val go = files.rootForTest().resolve(".rcs-media-1234567890").also { it.writeText("partial") }
        val interruptedRust = files.rootForTest().resolve("12345678-1234-1234-1234-123456789abc.bin.peppy-87654321-4321-4321-4321-cba987654321.tmp").also { it.writeText("partial") }
        assertEquals(rust.canonicalFile, files.safePath(rust))
        files.delete(rust)
        assertFalse(rust.exists())
        assertEquals(2, files.sweep())
        assertFalse(go.exists())
        assertFalse(interruptedRust.exists())
    }
    @Test fun `cleanup runs after success failure and cancellation`() {
        val files = privateFiles()
        val success = files.useTempFile { it.also { file -> file.writeText("plain") } }
        assertFalse(success.exists())

        var failed: java.io.File? = null
        runCatching {
            files.useTempFile { file ->
                failed = file
                throw IllegalStateException("failed")
            }
        }
        assertFalse(failed!!.exists())

        var cancelled: java.io.File? = null
        runCatching {
            files.useTempFile { file ->
                cancelled = file
                throw CancellationException("cancelled")
            }
        }
        assertFalse(cancelled!!.exists())
    }

    @Test fun `safe paths reject traversal and symlink escapes`() {
        val files = privateFiles()
        val escape = files.rootForTest().resolve("../outside")
        assertNull(files.safePath(escape))

        val target = Files.createTempFile("rcs-media-outside", ".tmp")
        val link = files.rootForTest().resolve("00000000-0000-0000-0000-000000000000")
        Files.createSymbolicLink(link.toPath(), target)

        assertNull(files.safePath(link))
        files.sweep()
        assertTrue(Files.exists(target))
    }

    @Test fun `sweep preserves leases supplied by another incarnation`() {
        val files = privateFiles()
        val active = files.createFile()
        val stale = files.createFile()

        assertEquals(1, files.sweep(setOf(active)))
        assertTrue(active.exists())
        assertFalse(stale.exists())
        assertEquals(1, files.sweep())
        assertFalse(active.exists())
    }

    private fun privateFiles(): RcsMediaPrivateFiles {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val files = RcsMediaPrivateFiles(context)
        files.sweep()
        return files
    }
}
