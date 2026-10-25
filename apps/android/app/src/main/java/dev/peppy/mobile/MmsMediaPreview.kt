package dev.peppy.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.ExifInterface
import android.media.MediaMetadataRetriever
import android.os.Build
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.math.max

/**
 * Best-effort, local-only MMS media inspection. A missing preview never prevents preserving the
 * copied original; callers must retain [MmsMediaPreviewResult.metadata] in either case.
 */
internal class MmsMediaPreview(
    private val maxJpegBytes: Int = MAX_JPEG_BYTES,
    private val retrieverFactory: MmsMediaMetadataRetrieverFactory = AndroidMmsMediaMetadataRetrieverFactory,
) {
    fun inspect(file: File, contentType: String?): MmsMediaPreviewResult {
        val type = contentType?.lowercase().orEmpty()
        return when {
            type.startsWith("image/") -> inspectImage(file, type)
            type.startsWith("video/") -> inspectTimedMedia(file, includePoster = true)
            type.startsWith("audio/") -> inspectTimedMedia(file, includePoster = false)
            else -> MmsMediaPreviewResult(MmsMediaMetadata(), null, MmsMediaPreviewUnavailable.UNSUPPORTED_MEDIA)
        }
    }

    private fun inspectImage(file: File, contentType: String): MmsMediaPreviewResult {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        return try {
            if (!matchesDeclaredImageType(file, contentType)) return unavailable(MmsMediaPreviewUnavailable.MALFORMED_MEDIA)
            BitmapFactory.decodeFile(file.path, bounds)
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return unavailable(MmsMediaPreviewUnavailable.MALFORMED_MEDIA)

            val orientation = imageOrientation(file)
            val dimensions = orientedDimensions(bounds.outWidth, bounds.outHeight, orientation)
            val options = BitmapFactory.Options().apply {
                inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight)
                inPreferredConfig = Bitmap.Config.ARGB_8888
            }
            val decoded = BitmapFactory.decodeFile(file.path, options)
                ?: return MmsMediaPreviewResult(MmsMediaMetadata(dimensions.first, dimensions.second), null, MmsMediaPreviewUnavailable.MALFORMED_MEDIA)
            try {
                val oriented = orient(decoded, orientation)
                if (oriented !== decoded) decoded.recycle()
                try {
                    val preview = encodePreview(oriented)
                    previewResult(MmsMediaMetadata(dimensions.first, dimensions.second), preview)
                } finally {
                    oriented.recycle()
                }
            } catch (_: IllegalArgumentException) {
                unavailable(MmsMediaPreviewUnavailable.MALFORMED_MEDIA, dimensions)
            }
        } catch (_: OutOfMemoryError) {
            unavailable(MmsMediaPreviewUnavailable.RESOURCE_LIMIT)
        } catch (_: Exception) {
            unavailable(MmsMediaPreviewUnavailable.MALFORMED_MEDIA)
        }
    }

    private fun inspectTimedMedia(file: File, includePoster: Boolean): MmsMediaPreviewResult {
        val retriever = retrieverFactory.create()
        return try {
            retriever.setDataSource(file)
            val width = retriever.metadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toPositiveIntOrNull()
            val height = retriever.metadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toPositiveIntOrNull()
            val durationMs = retriever.metadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toPositiveLongOrNull()
            val metadata = MmsMediaMetadata(width, height, durationMs)
            if (!includePoster) return MmsMediaPreviewResult(metadata, null, null)
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O_MR1) {
                return MmsMediaPreviewResult(metadata, null, MmsMediaPreviewUnavailable.UNSUPPORTED_MEDIA)
            }
            val poster = retriever.scaledFrameAtTime(MAX_EDGE)
                ?: return MmsMediaPreviewResult(metadata, null, MmsMediaPreviewUnavailable.MALFORMED_MEDIA)
            try {
                previewResult(metadata, encodePreview(poster))
            } finally {
                poster.recycle()
            }
        } catch (_: OutOfMemoryError) {
            return unavailable(MmsMediaPreviewUnavailable.RESOURCE_LIMIT)
        } catch (_: Exception) {
            return unavailable(MmsMediaPreviewUnavailable.MALFORMED_MEDIA)
        } finally {
            try {
                retriever.release()
            } catch (_: RuntimeException) {
                // A failed retriever still must not fail acquisition.
            }
        }
    }

    private fun imageOrientation(file: File): Int = try {
        ExifInterface(file.path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
    } catch (_: Exception) {
        ExifInterface.ORIENTATION_NORMAL
    }

    private fun matchesDeclaredImageType(file: File, contentType: String): Boolean {
        val header = ByteArray(12)
        val read = file.inputStream().use { it.read(header) }
        return when (contentType) {
            "image/jpeg", "image/jpg" -> read >= 2 && header[0] == 0xff.toByte() && header[1] == 0xd8.toByte()
            "image/png" -> read >= 8 && header.copyOfRange(0, 8).contentEquals(PNG_SIGNATURE)
            "image/gif" -> read >= 6 && (header.copyOfRange(0, 6).contentEquals(GIF87_SIGNATURE) || header.copyOfRange(0, 6).contentEquals(GIF89_SIGNATURE))
            "image/webp" -> read >= 12 && header.copyOfRange(0, 4).contentEquals(RIFF_SIGNATURE) && header.copyOfRange(8, 12).contentEquals(WEBP_SIGNATURE)
            else -> true
        }
    }

    private fun encodePreview(source: Bitmap): PreviewEncoding {
        val scaled = scale(source)
        try {
            for (quality in JPEG_QUALITIES) {
                // The scaled source is at most 480x480 ARGB, so this intermediate has a hard
                // 921,600-byte source bound even on encoders that write in oversized chunks.
                val output = ByteArrayOutputStream(maxJpegBytes)
                if (!scaled.compress(Bitmap.CompressFormat.JPEG, quality, output)) return PreviewEncoding.Failed
                if (output.size() <= maxJpegBytes) return PreviewEncoding.Jpeg(output.toByteArray())
            }
            return PreviewEncoding.Oversize
        } finally {
            if (scaled !== source) scaled.recycle()
        }
    }

    private fun scale(source: Bitmap): Bitmap {
        val longest = max(source.width, source.height)
        if (longest <= MAX_EDGE) return source
        val ratio = MAX_EDGE.toDouble() / longest
        return Bitmap.createScaledBitmap(source, (source.width * ratio).toInt().coerceAtLeast(1), (source.height * ratio).toInt().coerceAtLeast(1), true)
    }

    private fun orient(source: Bitmap, orientation: Int): Bitmap {
        val matrix = android.graphics.Matrix().apply {
            when (orientation) {
                ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> setScale(-1f, 1f)
                ExifInterface.ORIENTATION_ROTATE_180 -> setRotate(180f)
                ExifInterface.ORIENTATION_FLIP_VERTICAL -> setScale(1f, -1f)
                ExifInterface.ORIENTATION_TRANSPOSE -> { setRotate(90f); postScale(-1f, 1f) }
                ExifInterface.ORIENTATION_ROTATE_90 -> setRotate(90f)
                ExifInterface.ORIENTATION_TRANSVERSE -> { setRotate(-90f); postScale(-1f, 1f) }
                ExifInterface.ORIENTATION_ROTATE_270 -> setRotate(-90f)
                else -> return source
            }
        }
        return Bitmap.createBitmap(source, 0, 0, source.width, source.height, matrix, true)
    }

    private fun sampleSize(width: Int, height: Int): Int {
        var sample = 1
        while (width / sample > MAX_DECODE_EDGE || height / sample > MAX_DECODE_EDGE ||
            width.toLong() / sample / sample * height > MAX_DECODE_PIXELS) sample *= 2
        return sample
    }

    private fun orientedDimensions(width: Int, height: Int, orientation: Int): Pair<Int, Int> = when (orientation) {
        ExifInterface.ORIENTATION_ROTATE_90, ExifInterface.ORIENTATION_ROTATE_270,
        ExifInterface.ORIENTATION_TRANSPOSE, ExifInterface.ORIENTATION_TRANSVERSE -> height to width
        else -> width to height
    }

    private fun unavailable(reason: MmsMediaPreviewUnavailable, dimensions: Pair<Int, Int>? = null) = MmsMediaPreviewResult(
        MmsMediaMetadata(dimensions?.first, dimensions?.second), null, reason,
    )

    private fun previewResult(metadata: MmsMediaMetadata, encoding: PreviewEncoding) = when (encoding) {
        is PreviewEncoding.Jpeg -> MmsMediaPreviewResult(metadata, encoding.bytes, null)
        PreviewEncoding.Oversize -> MmsMediaPreviewResult(metadata, null, MmsMediaPreviewUnavailable.OVERSIZE_PREVIEW)
        PreviewEncoding.Failed -> MmsMediaPreviewResult(metadata, null, MmsMediaPreviewUnavailable.RESOURCE_LIMIT)
    }

    private fun String.toPositiveIntOrNull() = toIntOrNull()?.takeIf { it > 0 }
    private fun String.toPositiveLongOrNull() = toLongOrNull()?.takeIf { it > 0 }

    companion object {
        internal const val MAX_JPEG_BYTES = 512 * 1024
        const val MAX_EDGE = 480
        const val MAX_DECODE_EDGE = 960
        const val MAX_DECODE_PIXELS = 2_000_000L
        const val JPEG_QUALITY = 82
        val JPEG_QUALITIES = listOf(JPEG_QUALITY, 60, 40)
        val PNG_SIGNATURE = byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)
        val GIF87_SIGNATURE = "GIF87a".encodeToByteArray()
        val GIF89_SIGNATURE = "GIF89a".encodeToByteArray()
        val RIFF_SIGNATURE = "RIFF".encodeToByteArray()
        val WEBP_SIGNATURE = "WEBP".encodeToByteArray()
    }
}

internal data class MmsMediaMetadata(val width: Int? = null, val height: Int? = null, val durationMs: Long? = null)

internal data class MmsMediaPreviewResult(
    val metadata: MmsMediaMetadata,
    val jpegPreview: ByteArray?,
    val unavailable: MmsMediaPreviewUnavailable?,
)

internal enum class MmsMediaPreviewUnavailable { UNSUPPORTED_MEDIA, MALFORMED_MEDIA, RESOURCE_LIMIT, OVERSIZE_PREVIEW }

private sealed interface PreviewEncoding {
    class Jpeg(val bytes: ByteArray) : PreviewEncoding
    object Oversize : PreviewEncoding
    object Failed : PreviewEncoding
}

internal interface MmsMediaMetadataRetriever {
    fun setDataSource(file: File)
    fun metadata(key: Int): String?
    fun scaledFrameAtTime(edge: Int): Bitmap?
    fun release()
}

internal fun interface MmsMediaMetadataRetrieverFactory {
    fun create(): MmsMediaMetadataRetriever
}

private object AndroidMmsMediaMetadataRetrieverFactory : MmsMediaMetadataRetrieverFactory {
    override fun create(): MmsMediaMetadataRetriever = AndroidMmsMediaMetadataRetriever(MediaMetadataRetriever())
}

private class AndroidMmsMediaMetadataRetriever(private val delegate: MediaMetadataRetriever) : MmsMediaMetadataRetriever {
    override fun setDataSource(file: File) = delegate.setDataSource(file.path)
    override fun metadata(key: Int) = delegate.extractMetadata(key)
    override fun scaledFrameAtTime(edge: Int): Bitmap? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            delegate.getScaledFrameAtTime(-1, MediaMetadataRetriever.OPTION_CLOSEST_SYNC, edge, edge)
        } else null
    override fun release() = delegate.release()
}
