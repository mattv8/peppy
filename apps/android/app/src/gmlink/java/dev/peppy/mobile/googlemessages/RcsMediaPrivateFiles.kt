package dev.peppy.mobile.googlemessages

import android.content.Context
import java.io.File
import java.nio.file.Files
import java.util.UUID

/** Owns plaintext RCS transfer files below app-private no-backup storage. */
internal class RcsMediaPrivateFiles(context: Context) {
    private val root = File(context.noBackupFilesDir, DIRECTORY)
    private val canonicalRoot: File

    init {
        check(!Files.isSymbolicLink(root.toPath())) { "RCS media root must not be a symlink" }
        check(root.mkdirs() || root.isDirectory) { "Cannot create RCS media root" }
        canonicalRoot = root.canonicalFile
        check(canonicalRoot.parentFile == context.noBackupFilesDir.canonicalFile) { "Invalid RCS media root" }
        check(root.setReadable(false, false) && root.setWritable(false, false) && root.setExecutable(false, false))
        check(root.setReadable(true, true) && root.setWritable(true, true) && root.setExecutable(true, true))
    }

    fun createFile(): File {
        repeat(MAX_CREATE_ATTEMPTS) {
            val candidate = File(canonicalRoot, UUID.randomUUID().toString())
            if (candidate.createNewFile()) {
                restrictToOwner(candidate)
                return candidate
            }
        }
        throw IllegalStateException("Cannot allocate RCS media temporary file")
    }

    /** Returns a random, currently absent path suitable for a downloader that creates its own file. */
    fun createDestination(): File {
        repeat(MAX_CREATE_ATTEMPTS) {
            val candidate = File(canonicalRoot, UUID.randomUUID().toString())
            if (!candidate.exists()) return candidate
        }
        throw IllegalStateException("Cannot allocate RCS media temporary destination")
    }

    fun safePath(candidate: File): File? {
        val canonical = runCatching { candidate.canonicalFile }.getOrNull() ?: return null
        if (canonical.parentFile != canonicalRoot || Files.isSymbolicLink(candidate.toPath())) return null
        return canonical.takeIf { it.name.matches(OWNED_NAME) && (!it.exists() || it.isFile) }
    }

    fun delete(candidate: File) {
        safePath(candidate)?.delete()
    }

    fun <T> useTempFile(block: (File) -> T): T {
        val file = createFile()
        return try {
            block(file)
        } finally {
            delete(file)
        }
    }

    /** Deletes only inactive files directly owned by this helper's root. */
    fun sweep(active: Set<File> = emptySet()): Int {
        val protected = active.mapNotNull(::safePath).toSet()
        return canonicalRoot.listFiles()?.count { candidate ->
            val safe = safePath(candidate)
            safe != null && safe !in protected && candidate.delete()
        } ?: 0
    }

    internal fun rootForTest(): File = canonicalRoot

    private fun restrictToOwner(file: File) {
        check(file.setReadable(false, false) && file.setWritable(false, false) && file.setExecutable(false, false))
        check(file.setReadable(true, true) && file.setWritable(true, true))
    }

    private companion object {
        const val DIRECTORY = "rcs-media-tmp"
        const val MAX_CREATE_ATTEMPTS = 16
        const val UUID_PATTERN = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
        val OWNED_NAME = Regex("(?:$UUID_PATTERN(?:\\.bin(?:\\.peppy-$UUID_PATTERN\\.tmp)?)?|\\.rcs-media-[0-9]{1,20})")
    }
}
