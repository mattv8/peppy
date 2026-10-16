package dev.peppy.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.ExifInterface
import java.io.File
import java.nio.charset.StandardCharsets
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MmsMediaPreviewTest {
    @Test fun imagePreviewIsScaledTo480AndStripsExif() {
        val source = jpeg(1200, 600)
        val result = MmsMediaPreview().inspect(source, "image/jpeg")

        assertEquals(1200, result.metadata.width)
        assertEquals(600, result.metadata.height)
        assertNull(result.unavailable)
        val preview = requireNotNull(result.jpegPreview)
        val bitmap = BitmapFactory.decodeByteArray(preview, 0, preview.size)
        assertEquals(480, bitmap.width)
        assertEquals(240, bitmap.height)
        assertTrue("JPEG preview must not retain EXIF", !String(preview, StandardCharsets.US_ASCII).contains("Exif"))
    }

    @Test fun rotatedImageReportsOrientedDimensions() {
        val source = jpeg(24, 12)
        ExifInterface(source.path).apply {
            setAttribute(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_ROTATE_90.toString())
            saveAttributes()
        }

        val result = MmsMediaPreview().inspect(source, "image/jpeg")

        assertEquals(12, result.metadata.width)
        assertEquals(24, result.metadata.height)
        val preview = requireNotNull(result.jpegPreview)
        val bitmap = BitmapFactory.decodeByteArray(preview, 0, preview.size)
        assertEquals(12, bitmap.width)
        assertEquals(24, bitmap.height)
    }

    @Test fun malformedImageReturnsUnavailableWithoutThrowing() {
        val source = File.createTempFile("mms-preview", ".jpg").apply { writeBytes(byteArrayOf(1, 2, 3)) }
        val result = MmsMediaPreview().inspect(source, "image/jpeg")

        assertNull(result.jpegPreview)
        assertEquals(MmsMediaPreviewUnavailable.MALFORMED_MEDIA, result.unavailable)
    }

    @Test fun videoUsesInjectedRetrieverAndAlwaysReleasesIt() {
        val retriever = FakeRetriever(mapOf("18" to "1920", "19" to "1080", "9" to "1000"), Bitmap.createBitmap(20, 10, Bitmap.Config.ARGB_8888))
        val result = MmsMediaPreview { retriever }.inspect(File.createTempFile("mms-preview", ".mp4"), "video/mp4")

        assertEquals(1920, result.metadata.width)
        assertEquals(1080, result.metadata.height)
        assertEquals(1000L, result.metadata.durationMs)
        assertTrue(retriever.released)
        assertNull(result.unavailable)
        assertTrue(requireNotNull(result.jpegPreview).isNotEmpty())
    }

    @Test fun audioReturnsDurationWithoutPosterAndReleasesRetriever() {
        val retriever = FakeRetriever(mapOf("9" to "321"), null)
        val result = MmsMediaPreview { retriever }.inspect(File.createTempFile("mms-preview", ".mp3"), "audio/mpeg")

        assertEquals(321L, result.metadata.durationMs)
        assertNull(result.jpegPreview)
        assertNull(result.unavailable)
        assertTrue(retriever.released)
    }

    @Test fun retrieverFailureReturnsUnavailableAndReleasesIt() {
        val retriever = FakeRetriever(emptyMap(), null, failOnSetDataSource = true)
        val result = MmsMediaPreview { retriever }.inspect(File.createTempFile("mms-preview", ".mp4"), "video/mp4")

        assertEquals(MmsMediaPreviewUnavailable.MALFORMED_MEDIA, result.unavailable)
        assertTrue(retriever.released)
    }

    @Test fun resourceLimitedPosterReturnsUnavailableAndReleasesRetriever() {
        val retriever = FakeRetriever(emptyMap(), null, failOnFrame = true)
        val result = MmsMediaPreview { retriever }.inspect(File.createTempFile("mms-preview", ".mp4"), "video/mp4")

        assertEquals(MmsMediaPreviewUnavailable.RESOURCE_LIMIT, result.unavailable)
        assertTrue(retriever.released)
    }

    private fun jpeg(width: Int, height: Int): File = File.createTempFile("mms-preview", ".jpg").also { file ->
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        try {
            file.outputStream().use { bitmap.compress(Bitmap.CompressFormat.JPEG, 90, it) }
        } finally {
            bitmap.recycle()
        }
    }

    private class FakeRetriever(
        private val values: Map<String, String>,
        private val frame: Bitmap?,
        private val failOnSetDataSource: Boolean = false,
        private val failOnFrame: Boolean = false,
    ) : MmsMediaMetadataRetriever {
        var released = false
        override fun setDataSource(file: File) {
            if (failOnSetDataSource) error("broken")
        }
        override fun metadata(key: Int) = values[key.toString()]
        override fun scaledFrameAtTime(edge: Int): Bitmap? {
            if (failOnFrame) throw OutOfMemoryError("simulated oversized frame")
            return frame
        }
        override fun release() { released = true }
    }
}
