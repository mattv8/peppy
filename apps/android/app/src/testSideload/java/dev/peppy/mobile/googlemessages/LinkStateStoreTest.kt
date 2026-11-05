package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.security.AesGcmSecretBox
import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.After
import org.junit.Test

class LinkStateStoreTest {
    private val directories = mutableListOf<File>()

    @After
    fun cleanUp() {
        directories.forEach(File::deleteRecursively)
    }

    @Test
    fun `round trips sealed state`() {
        val directory = directory()
        val store = store(directory)
        val state = byteArrayOf(1, 2, 3)

        store.write(state)

        assertArrayEquals(state, store.read())
    }

    @Test
    fun `missing state is empty`() {
        val directory = directory()

        assertArrayEquals(ByteArray(0), store(directory).read())
    }

    @Test
    fun `corrupt or truncated state is unreadable`() {
        val directory = directory()
        val store = store(directory)
        store.write(byteArrayOf(1))
        stateFile(directory).writeBytes(byteArrayOf(1))

        assertUnreadable(store)
        stateFile(directory).writeBytes(byteArrayOf(1, 2))

        assertUnreadable(store)
    }

    @Test
    fun `wrong key leaves state in place`() {
        val directory = directory()
        val store = store(directory)
        store.write(byteArrayOf(1))
        val file = stateFile(directory)

        assertUnreadable(store(directory))

        assertTrue(file.isFile)
    }

    @Test
    fun `write syncs file before atomic replacement and directory afterward`() {
        val directory = directory()
        val fileSystem = RecordingFileSystem()

        LinkStateStore(directory, box(), fileSystem).write(byteArrayOf(1))

        assertEquals(
            listOf("create-temp", "write-and-sync", "replace", "sync-directory", "delete-temp"),
            fileSystem.operations,
        )
    }

    @Test
    fun `interrupted replacement keeps old state`() {
        val directory = directory()
        val fileSystem = RecordingFileSystem()
        val store = LinkStateStore(directory, box(), fileSystem)
        store.write(byteArrayOf(1))
        fileSystem.failReplacement = true

        assertWriteFailed { store.write(byteArrayOf(2)) }

        fileSystem.failReplacement = false
        assertArrayEquals(byteArrayOf(1), store.read())
        assertTrue(directory.listFiles().orEmpty().none { it.name.endsWith(".tmp") })
    }

    @Test
    fun `durability failure is reported rather than swallowed`() {
        val directory = directory()
        val fileSystem = RecordingFileSystem(failDirectorySync = true)

        assertWriteFailed { LinkStateStore(directory, box(), fileSystem).write(byteArrayOf(1)) }

        assertEquals(
            listOf("create-temp", "write-and-sync", "replace", "sync-directory", "delete-temp"),
            fileSystem.operations,
        )
    }

    @Test
    fun `successive writes use unique temporary files`() {
        val directory = directory()
        val fileSystem = RecordingFileSystem()
        val store = LinkStateStore(directory, box(), fileSystem)

        store.write(byteArrayOf(1))
        store.write(byteArrayOf(2))

        assertEquals(2, fileSystem.temporaryFiles.size)
        assertNotEquals(fileSystem.temporaryFiles[0].name, fileSystem.temporaryFiles[1].name)
    }

    @Test
    fun `wipe removes state and temporary files`() {
        val directory = directory()
        val store = store(directory)
        store.write(byteArrayOf(1))
        File(directory, "${stateFile(directory).name}.left.tmp").writeBytes(byteArrayOf(2))

        store.wipe()

        assertFalse(stateFile(directory).exists())
        assertTrue(directory.listFiles().orEmpty().none { it.name.startsWith("${stateFile(directory).name}.") })
    }

    @Test
    fun `state file stays under supplied directory`() {
        val directory = directory()
        store(directory).write(byteArrayOf(1))

        assertEquals(directory.canonicalFile, stateFile(directory).parentFile!!.canonicalFile)
    }

    private fun assertUnreadable(store: LinkStateStore) {
        assertEquals("link_state_unreadable", assertThrows(IllegalStateException::class.java) { store.read() }.message)
    }

    private fun assertWriteFailed(block: () -> Unit) {
        assertEquals("link_state_write_failed", assertThrows(IllegalStateException::class.java, block).message)
    }

    private fun directory(): File = Files.createTempDirectory("link-state-store").toFile().also(directories::add)

    private fun stateFile(directory: File): File = directory.resolve("google-messages-link-state.bin")

    private fun store(directory: File): LinkStateStore = LinkStateStore(directory, box(), RecordingFileSystem())

    private fun box(): AesGcmSecretBox {
        var key: SecretKey? = null
        return AesGcmSecretBox { create ->
            key ?: if (create) KeyGenerator.getInstance("AES").apply { init(256) }.generateKey().also { key = it } else null
        }
    }

    private class RecordingFileSystem(
        private val failDirectorySync: Boolean = false,
    ) : LinkStateFileSystem {
        val operations = mutableListOf<String>()
        val temporaryFiles = mutableListOf<File>()
        var failReplacement = false

        override fun createTemporaryFile(directory: File, prefix: String, suffix: String): File {
            operations += "create-temp"
            return File.createTempFile(prefix, suffix, directory).also(temporaryFiles::add)
        }

        override fun writeAndSync(file: File, value: ByteArray) {
            operations += "write-and-sync"
            FileOutputStream(file).use { output ->
                output.write(value)
                output.fd.sync()
            }
        }

        override fun replace(source: File, destination: File) {
            operations += "replace"
            if (failReplacement) throw IllegalStateException("simulated-crash")
            check(source.renameTo(destination))
        }

        override fun syncDirectory(directory: File) {
            operations += "sync-directory"
            if (failDirectorySync) throw IllegalStateException("simulated-fsync-failure")
        }

        override fun deleteTemporary(file: File) {
            operations += "delete-temp"
            file.delete()
        }
    }
}
