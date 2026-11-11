package dev.peppy.mobile.mms

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.os.Build
import androidx.exifinterface.media.ExifInterface
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.security.MessageDigest

/** Bounded, durable image fitting used only before a carrier send permit is acquired. */
internal class MmsImageFitter(
    private val root: File,
    private val beforePreparation: (() -> Boolean)? = null,
    private val publishMetadata: ((File, File) -> Unit)? = null,
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
                null -> return transient(directory)
                false -> Unit
            }
            directory.deleteRecursively()
        }
        if (!directory.exists() && !directory.mkdirs()) return MmsFitResult.Transient
        val attempts = IntArray(parts.size)
        var preparations = 0
        val selected = parts.map { part ->
            if (!part.resizable || part.file.length() <= SMALL_IMAGE_BYTES) part else null
        }.toMutableList()
        var nextPart = 0
        while (true) {
            val missing = selected.indexOfFirst { it == null }
            if (missing >= 0) {
                if (preparations >= MAX_PREPARATIONS_PER_COMMAND || beforePreparation?.invoke() == false) return transient(directory)
                preparations++
                when (val candidate = nextCandidate(parts[missing], missing, attempts, directory)) {
                    is EncodeResult.Success -> selected[missing] = parts[missing].copy(file = candidate.file, mediaType = "image/jpeg", name = jpegName(parts[missing].name))
                    EncodeResult.Invalid -> return impossible(directory)
                    EncodeResult.Transient -> return transient(directory)
                    EncodeResult.Retryable -> return transient(directory)
                    EncodeResult.Exhausted -> return impossible(directory)
                }
                continue
            }
            when (fits(selected.filterNotNull(), maximumPduBytes, measure)) {
                true -> return save(directory, fingerprint, selected.filterNotNull())
                null -> return transient(directory)
                false -> Unit
            }
            var advanced = false
            for (offset in parts.indices) {
                val index = (nextPart + offset) % parts.size
                if (!parts[index].resizable) continue
                if (attempts[index] >= LADDER.size) continue
                if (preparations >= MAX_PREPARATIONS_PER_COMMAND || beforePreparation?.invoke() == false) return transient(directory)
                preparations++
                when (val candidate = nextCandidate(parts[index], index, attempts, directory)) {
                    is EncodeResult.Success -> {
                        selected[index] = parts[index].copy(file = candidate.file, mediaType = "image/jpeg", name = jpegName(parts[index].name))
                        nextPart = index + 1
                        advanced = true
                        break
                    }
                    EncodeResult.Invalid -> return impossible(directory)
                    EncodeResult.Transient -> return transient(directory)
                    EncodeResult.Retryable -> return transient(directory)
                    EncodeResult.Exhausted -> Unit
                }
            }
            if (!advanced) return impossible(directory)
        }
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
            publishMetadata?.invoke(temporary, metadata) ?: Files.move(temporary.toPath(), metadata.toPath(), ATOMIC_MOVE, REPLACE_EXISTING)
        }.isSuccess) MmsFitResult.Fitted(parts) else transient(directory)
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

    private fun nextCandidate(part: MmsFitPart, index: Int, attempts: IntArray, directory: File): EncodeResult {
        var retryableFailure = false
        while (attempts[index] < LADDER.size) {
            val (edge, quality) = LADDER[attempts[index]++]
            when (val result = encode(part.file, edge, quality, File(directory, "$index-$edge-$quality.jpg"))) {
                EncodeResult.Retryable -> retryableFailure = true
                else -> return result
            }
        }
        return if (retryableFailure) EncodeResult.Retryable else EncodeResult.Exhausted
    }

    private fun impossible(directory: File): MmsFitResult { directory.deleteRecursively(); return MmsFitResult.Impossible }
    private fun transient(directory: File): MmsFitResult { directory.deleteRecursively(); return MmsFitResult.Transient }

    private fun encode(source: File, edge: Int, quality: Int, destination: File): EncodeResult {
        encoder?.let {
            return try { EncodeResult.Success(it(source, edge, quality, destination)) }
            catch (_: OutOfMemoryError) { EncodeResult.Transient }
            catch (_: java.io.IOException) { EncodeResult.Transient }
            catch (_: IllegalArgumentException) { EncodeResult.Invalid }
            catch (_: RuntimeException) { EncodeResult.Retryable }
        }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P && source.extension.lowercase() in setOf("heic", "heif")) return EncodeResult.Invalid
        return try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(source.path, bounds)
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return EncodeResult.Invalid
            val sample = sampleSize(bounds.outWidth, bounds.outHeight)
            val bitmap = BitmapFactory.decodeFile(source.path, BitmapFactory.Options().apply { inSampleSize = sample }) ?: return EncodeResult.Invalid
            var oriented: Bitmap? = null
            var resized: Bitmap? = null
            try {
                oriented = orient(bitmap, orientation(source))
                val scale = minOf(1f, edge.toFloat() / maxOf(oriented.width, oriented.height))
                resized = if (scale < 1f) Bitmap.createScaledBitmap(oriented, (oriented.width * scale).toInt().coerceAtLeast(1), (oriented.height * scale).toInt().coerceAtLeast(1), true) else oriented
                destination.outputStream().use { resized.compress(Bitmap.CompressFormat.JPEG, quality, it) }
                destination.takeIf { it.isFile && it.length() > 0 }?.let(EncodeResult::Success) ?: EncodeResult.Invalid
            } finally {
                if (resized != null && resized !== oriented) resized.recycle()
                if (oriented != null && oriented !== bitmap) oriented.recycle()
                bitmap.recycle()
            }
        } catch (_: OutOfMemoryError) { EncodeResult.Transient }
        catch (_: java.io.IOException) { EncodeResult.Transient }
        catch (_: RuntimeException) { EncodeResult.Invalid }
    }

    private fun orientation(file: File): Int = runCatching { ExifInterface(file.path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL) }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
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
    private fun jpegName(name: String?): String {
        val base = name?.substringBeforeLast('.', name)?.ifEmpty { "image" } ?: "image"
        val bounded = StringBuilder()
        var offset = 0
        while (offset < base.length) {
            val codePoint = base.codePointAt(offset)
            val character = String(Character.toChars(codePoint))
            if (bounded.toString().toByteArray(Charsets.UTF_8).size + character.toByteArray(Charsets.UTF_8).size > MAX_JPEG_BASENAME_BYTES) break
            bounded.append(character)
            offset += Character.charCount(codePoint)
        }
        return "${bounded.ifEmpty { "image" }}.jpg"
    }

    companion object {
        private const val SMALL_IMAGE_BYTES = 90L * 1024
        private const val MAX_DECODE_PIXELS = 4_000_000L
        private const val MAX_JPEG_BASENAME_BYTES = 251
        private const val MAX_MMS_ATTACHMENTS = 10
        private const val MAX_LADDER_STEPS = 6
        const val MAX_PREPARATIONS_PER_COMMAND = MAX_MMS_ATTACHMENTS * MAX_LADDER_STEPS
        private val LADDER = listOf(1600 to 82, 1280 to 75, 1024 to 68, 800 to 60, 640 to 50, 480 to 40)
        fun fingerprint(parts: List<MmsFitPart>, cap: MmsLimits, messageKey: String): String = MessageDigest.getInstance("SHA-256").digest(
            buildString { append(cap.maximumPduBytes).append('|').append(messageKey); parts.forEach { append('|').append(it.attachmentId).append(':').append(it.file.length()).append(':').append(it.mediaType) } }.toByteArray(),
        ).joinToString("") { "%02x".format(it) }
    }
}

private sealed interface EncodeResult {
    data class Success(val file: File) : EncodeResult
    data object Invalid : EncodeResult
    data object Transient : EncodeResult
    data object Retryable : EncodeResult
    data object Exhausted : EncodeResult
}

internal data class MmsFitPart(val file: File, val mediaType: String, val name: String?, val attachmentId: String? = null) {
    val resizable get() = mediaType.lowercase() in setOf("image/jpeg", "image/png", "image/webp", "image/heic", "image/heif")
}
internal sealed interface MmsFitResult {
    data class Fitted(val parts: List<MmsFitPart>) : MmsFitResult
    data object Impossible : MmsFitResult
    data object Transient : MmsFitResult
}
