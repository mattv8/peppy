package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.security.AesGcmSecretBox
import java.io.File
import java.nio.file.Files
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class LinkStateStoreTest {
    private val directories = mutableListOf<File>()

    @After
    fun cleanUp() {
        LinkStateStoreTestHooks.beforeRename = null
        directories.forEach(File::deleteRecursively)
    }

    @Test
    fun `round trips sealed state`() {
        val store = LinkStateStore(directory(), box())
        val state = byteArrayOf(1, 2, 3)

        store.write(state)

        assertArrayEquals(state, store.read())
    }

    @Test
    fun `missing state is empty`() {
        assertArrayEquals(ByteArray(0), LinkStateStore(directory(), box()).read())
    }

    @Test
    fun `corrupt or truncated state is unreadable`() {
        val store = LinkStateStore(directory(), box())
        store.write(byteArrayOf(1))
        stateFile().writeBytes(byteArrayOf(1))

        assertUnreadable(store)
        stateFile().writeBytes(byteArrayOf(1, 2))

        assertUnreadable(store)
    }

    @Test
    fun `wrong key leaves state in place`() {
        val store = LinkStateStore(directory(), box())
        store.write(byteArrayOf(1))
        val file = stateFile()

        assertUnreadable(LinkStateStore(file.parentFile!!, box()))

        assertTrue(file.isFile)
    }

    @Test
    fun `interrupted replacement keeps old state`() {
        val store = LinkStateStore(directory(), box())
        store.write(byteArrayOf(1))
        LinkStateStoreTestHooks.beforeRename = { throw IllegalStateException("simulated_crash") }

        assertThrows(IllegalStateException::class.java) { store.write(byteArrayOf(2)) }

        LinkStateStoreTestHooks.beforeRename = null
        assertArrayEquals(byteArrayOf(1), store.read())
    }

    @Test
    fun `wipe removes state and temporary files`() {
        val store = LinkStateStore(directory(), box())
        store.write(byteArrayOf(1))
        File(stateFile().parentFile!!, "${stateFile().name}.left.tmp").writeBytes(byteArrayOf(2))

        store.wipe()

        assertFalse(stateFile().exists())
        assertTrue(stateFile().parentFile!!.listFiles().orEmpty().none { it.name.startsWith("${stateFile().name}.") })
    }

    @Test
    fun `state file stays under supplied directory`() {
        val directory = directory()
        LinkStateStore(directory, box()).write(byteArrayOf(1))

        assertEquals(directory.canonicalFile, stateFile().parentFile!!.canonicalFile)
    }

    private fun assertUnreadable(store: LinkStateStore) {
        assertEquals("link_state_unreadable", assertThrows(IllegalStateException::class.java) { store.read() }.message)
    }

    private fun directory(): File = Files.createTempDirectory("link-state-store").toFile().also(directories::add)

    private fun stateFile(): File = directories.single().resolve("google-messages-link-state.bin")

    private fun box(): AesGcmSecretBox {
        var key: SecretKey? = null
        return AesGcmSecretBox { create ->
            key ?: if (create) KeyGenerator.getInstance("AES").apply { init(256) }.generateKey().also { key = it } else null
        }
    }
}
