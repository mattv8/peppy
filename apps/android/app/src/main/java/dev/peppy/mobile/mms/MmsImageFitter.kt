package dev.peppy.mobile.mms

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import androidx.exifinterface.media.ExifInterface
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.security.MessageDigest

/** Bounded, durable image fitting used only before a carrier send permit is acquired. */
internal class MmsImageFitter(
    private val root: File,
    private val encoder: ((File, Int, Int, File) -> File)? = null,
) {
    fun fit(
        commandId: String,
        fingerprint: String,
        maximumPduBytes: Long,
        parts: List<MmsFitPart>,
        measure: (List<MmsFitPart>) -> Long,
    ): MmsFitResult {
        if (parts.none { it.resizable }) return MmsFitResult.Impossible
        val directory = File(root, commandId)
        load(directory, fingerprint, parts)?.let { cached ->
            when (fits(cached, maximumPduBytes, measure)) {
                true -> return MmsFitResult.Fitted(cached)
                null -> return MmsFitResult.Transient
                false -> Unit
            }
            directory.deleteRecursively()
        }
        if (!directory.exists() && !directory.mkdirs()) return MmsFitResult.Transient
        var encodingFailed = false
        val candidates = parts.mapIndexed { index, part ->
            val encoded = if (part.resizable) LADDER.mapNotNull { (edge, quality) ->
                encode(part.file, edge, quality, File(directory, "$index-$edge-$quality.jpg"))
                    ?.let { part.copy(file = it, mediaType = "image/jpeg", name = jpegName(part.name)) }
                    ?: run { encodingFailed = true; null }
            } else emptyList()
            when {
                !part.resizable -> listOf(part)
                part.file.length() <= SMALL_IMAGE_BYTES -> listOf(part) + encoded
                else -> encoded
            }
        }
        if (candidates.any { it.isEmpty() }) return MmsFitResult.Transient
        var selected = candidates.map { it.first() }
        when (fits(selected, maximumPduBytes, measure)) {
            true -> return save(directory, fingerprint, selected)
            null -> return MmsFitResult.Transient
            false -> Unit
        }
        for (partIndex in selected.indices) {
            for (candidate in candidates[partIndex].drop(1)) {
                selected = selected.toMutableList().also { it[partIndex] = candidate }
                when (fits(selected, maximumPduBytes, measure)) {
                    true -> return save(directory, fingerprint, selected)
                    null -> return MmsFitResult.Transient
                    false -> Unit
                }
            }
        }
        directory.deleteRecursively()
        return if (encodingFailed) MmsFitResult.Transient else MmsFitResult.Impossible
    }

    /** null means the PDU could not be measured, so oversize is not proven. */
    private fun fits(parts: List<MmsFitPart>, maximumPduBytes: Long, measure: (List<MmsFitPart>) -> Long): Boolean? =
        runCatching { measure(parts) <= maximumPduBytes }.getOrNull()

    private fun save(directory: File, fingerprint: String, parts: List<MmsFitPart>): MmsFitResult {
        val metadata = File(directory, "fit")
        val temporary = File(directory, ".fit-${System.nanoTime()}.tmp")
        val body = buildString {
            append(fingerprint).append('\n')
            parts.forEachIndexed { index, part ->
                val fitted = part.file.name.startsWith("$index-")
                append(index).append('\t').append(if (fitted) part.file.name else "").append('\t')
                    .append(part.mediaType).append('\t').append(part.name ?: "").append('\n')
            }
        }
        return if (runCatching {
            temporary.writeText(body)
            Files.move(temporary.toPath(), metadata.toPath(), ATOMIC_MOVE, REPLACE_EXISTING)
        }.isSuccess) MmsFitResult.Fitted(parts) else MmsFitResult.Transient
    }

    private fun load(directory: File, fingerprint: String, current: List<MmsFitPart>): List<MmsFitPart>? = runCatching {
        val rows = File(directory, "fit").readLines()
        if (rows.firstOrNull() != fingerprint || rows.size != current.size + 1) return null
        rows.drop(1).mapIndexed { index, row ->
            val fields = row.split('\t', ignoreCase = false, limit = 4)
            require(fields.size == 4)
            require(fields[0].toInt() == index)
            val original = current[index]
            if (fields[1].isEmpty()) original else original.copy(
                file = File(directory, fields[1]).also { require(it.isFile) },
                mediaType = fields[2],
                name = fields[3].ifEmpty { null },
            )
        }
    }.getOrNull()

    private fun encode(source: File, edge: Int, quality: Int, destination: File): File? {
        encoder?.let { return runCatching { it(source, edge, quality, destination) }.getOrNull() }
        return try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(source.path, bounds)
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
            val sample = sampleSize(bounds.outWidth, bounds.outHeight)
            val bitmap = BitmapFactory.decodeFile(source.path, BitmapFactory.Options().apply { inSampleSize = sample }) ?: return null
            val oriented = orient(bitmap, orientation(source))
            val scale = minOf(1f, edge.toFloat() / maxOf(oriented.width, oriented.height))
            val resized = if (scale < 1f) Bitmap.createScaledBitmap(oriented, (oriented.width * scale).toInt().coerceAtLeast(1), (oriented.height * scale).toInt().coerceAtLeast(1), true) else oriented
            destination.outputStream().use { resized.compress(Bitmap.CompressFormat.JPEG, quality, it) }
            if (resized !== oriented) resized.recycle()
            if (oriented !== bitmap) oriented.recycle()
            bitmap.recycle()
            destination.takeIf { it.isFile && it.length() > 0 }
        } catch (_: OutOfMemoryError) { null } catch (_: RuntimeException) { null } catch (_: java.io.IOException) { null }
    }

    private fun orientation(file: File): Int = runCatching { ExifInterface(file).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL) }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
    internal fun orient(bitmap: Bitmap, orientation: Int): Bitmap {
        val matrix = orientationMatrix(orientation) ?: return bitmap
        return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
    }
    internal fun orientationMatrix(orientation: Int): Matrix? = Matrix().apply { when (orientation) {
            ExifInterface.ORIENTATION_ROTATE_90 -> setRotate(90f)
            ExifInterface.ORIENTATION_ROTATE_180 -> setRotate(180f)
            ExifInterface.ORIENTATION_ROTATE_270 -> setRotate(270f)
            ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> setScale(-1f, 1f)
            ExifInterface.ORIENTATION_FLIP_VERTICAL -> setScale(1f, -1f)
            ExifInterface.ORIENTATION_TRANSPOSE -> { setRotate(90f); postScale(-1f, 1f) }
            ExifInterface.ORIENTATION_TRANSVERSE -> { setRotate(270f); postScale(-1f, 1f) }
            else -> return null
        } }
    private fun sampleSize(width: Int, height: Int): Int {
        var sample = 1
        while ((width / sample).toLong() * (height / sample) > MAX_DECODE_PIXELS) sample *= 2
        return sample
    }
    private fun jpegName(name: String?) = name?.substringBeforeLast('.', name) + ".jpg"

    companion object {
        private const val SMALL_IMAGE_BYTES = 90L * 1024
        private const val MAX_DECODE_PIXELS = 4_000_000L
        private val LADDER = listOf(1600 to 82, 1280 to 75, 1024 to 68, 800 to 60, 640 to 50, 480 to 40)
        fun fingerprint(parts: List<MmsFitPart>, cap: MmsLimits, messageKey: String): String = MessageDigest.getInstance("SHA-256").digest(
            buildString { append(cap.maximumPduBytes).append('|').append(messageKey); parts.forEach { append('|').append(it.attachmentId).append(':').append(it.file.length()).append(':').append(it.mediaType) } }.toByteArray(),
        ).joinToString("") { "%02x".format(it) }
    }
}

internal data class MmsFitPart(val file: File, val mediaType: String, val name: String?, val attachmentId: String? = null) {
    val resizable get() = mediaType.lowercase() in setOf("image/jpeg", "image/png", "image/webp", "image/heic", "image/heif")
}
internal sealed interface MmsFitResult {
    data class Fitted(val parts: List<MmsFitPart>) : MmsFitResult
    data object Impossible : MmsFitResult
    data object Transient : MmsFitResult
}
