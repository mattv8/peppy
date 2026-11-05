package dev.peppy.mobile.googlemessages

import android.system.Os
import android.system.OsConstants
import dev.peppy.mobile.security.KeystoreSecretBox
import dev.peppy.mobile.security.SecretBox
import java.io.File
import java.io.FileOutputStream

class LinkStateStore internal constructor(
    private val directory: File,
    private val box: SecretBox,
    private val fileSystem: LinkStateFileSystem,
) {
    constructor(
        directory: File,
        box: SecretBox = KeystoreSecretBox,
    ) : this(directory, box, AndroidLinkStateFileSystem)

    private val state = File(directory, STATE_FILE_NAME)

    fun read(): ByteArray {
        if (!state.exists()) return ByteArray(0)
        if (!state.isFile) throw IllegalStateException(UNREADABLE)
        val sealed = try {
            state.readText(Charsets.UTF_8)
        } catch (_: Exception) {
            throw IllegalStateException(UNREADABLE)
        }
        return try {
            box.open(PURPOSE, sealed) ?: throw IllegalStateException(UNREADABLE)
        } catch (_: Exception) {
            throw IllegalStateException(UNREADABLE)
        }
    }

    @Synchronized
    fun write(value: ByteArray) {
        try {
            check(directory.exists() || directory.mkdirs()) { DIRECTORY_UNAVAILABLE }
            val temporary = fileSystem.createTemporaryFile(directory, "$STATE_FILE_NAME.", ".tmp")
            try {
                val sealed = box.seal(PURPOSE, value).toByteArray(Charsets.UTF_8)
                fileSystem.writeAndSync(temporary, sealed)
                fileSystem.replace(temporary, state)
                fileSystem.syncDirectory(directory)
            } finally {
                fileSystem.deleteTemporary(temporary)
            }
        } catch (_: Exception) {
            throw IllegalStateException(WRITE_FAILED)
        }
    }

    @Synchronized
    fun wipe() {
        try {
            if (!directory.exists()) return
            var changed = false
            if (state.exists()) {
                check(state.delete()) { WIPE_FAILED }
                changed = true
            }
            directory.listFiles { file ->
                file.name.startsWith("$STATE_FILE_NAME.") && file.name.endsWith(".tmp")
            }.orEmpty().forEach { temporary ->
                check(temporary.delete()) { WIPE_FAILED }
                changed = true
            }
            if (changed) fileSystem.syncDirectory(directory)
        } catch (_: Exception) {
            throw IllegalStateException(WIPE_FAILED)
        }
    }

    private companion object {
        const val PURPOSE = "google-messages-link-v1"
        const val STATE_FILE_NAME = "google-messages-link-state.bin"
        const val DIRECTORY_UNAVAILABLE = "link_state_directory_unavailable"
        const val UNREADABLE = "link_state_unreadable"
        const val WRITE_FAILED = "link_state_write_failed"
        const val WIPE_FAILED = "link_state_wipe_failed"
    }
}

internal interface LinkStateFileSystem {
    fun createTemporaryFile(directory: File, prefix: String, suffix: String): File

    fun writeAndSync(file: File, value: ByteArray)

    fun replace(source: File, destination: File)

    fun syncDirectory(directory: File)

    fun deleteTemporary(file: File)
}

private object AndroidLinkStateFileSystem : LinkStateFileSystem {
    override fun createTemporaryFile(directory: File, prefix: String, suffix: String): File =
        File.createTempFile(prefix, suffix, directory)

    override fun writeAndSync(file: File, value: ByteArray) {
        FileOutputStream(file).use { output ->
            output.write(value)
            output.fd.sync()
        }
    }

    override fun replace(source: File, destination: File) {
        check(source.renameTo(destination)) { "link_state_replace_failed" }
    }

    override fun syncDirectory(directory: File) {
        val descriptor = Os.open(directory.absolutePath, OsConstants.O_RDONLY, 0)
        try {
            Os.fsync(descriptor)
        } finally {
            Os.close(descriptor)
        }
    }

    override fun deleteTemporary(file: File) {
        file.delete()
    }
}
