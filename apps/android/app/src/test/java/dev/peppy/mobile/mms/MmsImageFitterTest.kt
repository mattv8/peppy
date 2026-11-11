package dev.peppy.mobile.mms

import android.graphics.Bitmap
import androidx.exifinterface.media.ExifInterface
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsImageFitterTest {
    @Test
    fun retriesSmallerEncodingsUntilTheMeasuredMessageFits() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, maxEdge, quality, destination ->
            destination.apply { writeBytes(ByteArray(maxEdge / 10 + quality)) }
        }
        val result = fitter.fit(
            "command", "fingerprint", 300,
            listOf(MmsFitPart(source, "image/jpeg", "photo.jpg")),
        ) { parts -> parts.sumOf { it.file.length() } }

        assertTrue(result is MmsFitResult.Fitted)
        assertTrue((result as MmsFitResult.Fitted).parts.single().file.length() <= 300)
    }

    @Test
    fun returnsImpossibleWhenEveryEncodingExceedsTheMeasuredLimit() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, _, _, destination ->
            destination.apply { writeBytes(ByteArray(400)) }
        }

        val result = fitter.fit(
            "command", "fingerprint", 300,
            listOf(MmsFitPart(source, "image/jpeg", "photo.jpg")),
        ) { 400L }

        assertTrue(result is MmsFitResult.Impossible)
    }

    @Test
    fun reusesAValidCachedFitWithoutEncodingAgain() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        var encodes = 0
        val fitter = MmsImageFitter(root) { _, _, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(100)) }
        }
        val parts = listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))
        val measure: (List<MmsFitPart>) -> Long = { it.sumOf { part -> part.file.length() } }

        fitter.fit("command", "fingerprint", 300, parts, measure)
        val firstCount = encodes
        fitter.fit("command", "fingerprint", 300, parts, measure)

        assertTrue(firstCount > 0)
        assertTrue(encodes == firstCount)
    }

    @Test fun cachedMeasurementFailureDeletesFittedPlaintext() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, _, _, destination -> destination.apply { writeBytes(ByteArray(100)) } }
        assertTrue(fitter.fit("command", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) { 100L } is MmsFitResult.Fitted)

        assertTrue(fitter.fit("command", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) { error("measurement failed") } is MmsFitResult.Transient)
        assertFalse(File(root, "command").exists())
    }

    @Test fun metadataPublicationFailureDeletesFittedPlaintext() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(
            root,
            encoder = { _, _, _, destination -> destination.apply { writeBytes(ByteArray(100)) } },
            publishMetadata = { _, _ -> error("metadata publication failed") },
        )

        assertTrue(fitter.fit("command", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) { 100L } is MmsFitResult.Transient)
        assertFalse(File(root, "command").exists())
    }

    @Test
    fun generatesOnlyTheFirstLadderCandidateThatFits() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        var encodes = 0
        val fitter = MmsImageFitter(root) { _, _, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(100)) }
        }

        val result = fitter.fit("lazy", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) {
            it.sumOf { part -> part.file.length() }
        }

        assertTrue(result is MmsFitResult.Fitted)
        assertEquals("a fitting first candidate must not prepare the remaining ladder", 1, encodes)
    }

    @Test
    fun usesABoundedJpegFallbackNameForNamelessAndLongUtf8Sources() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, _, _, destination -> destination.apply { writeBytes(ByteArray(100)) } }
        val names = listOf<String?>(null, "é".repeat(200) + ".png")

        names.forEachIndexed { index, name ->
            val result = fitter.fit("name-$index", "fingerprint-$index", 300, listOf(MmsFitPart(source, "image/jpeg", name))) {
                it.sumOf { part -> part.file.length() }
            } as MmsFitResult.Fitted
            assertEquals(".jpg", result.parts.single().name?.takeLast(4))
            assertTrue(result.parts.single().name!!.toByteArray(Charsets.UTF_8).size <= 255)
            assertTrue(result.parts.single().name != "null.jpg")
        }
    }

    @Test
    fun treatsDeterministicDecodeFailureAsPermanent() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, _, _, _ -> throw IllegalArgumentException("malformed image") }

        assertTrue(fitter.fit("invalid", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) { 400L } is MmsFitResult.Impossible)
    }

    @Test
    fun defersBeforePreparationWhenTheDispatcherBudgetIsExhausted() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        var encodes = 0
        val fitter = MmsImageFitter(root, beforePreparation = { false }) { _, _, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(400)) }
        }

        val result = fitter.fit("budget", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg"))) { 400L }

        assertTrue(result is MmsFitResult.Transient)
        assertEquals(0, encodes)
    }

    @Test
    fun fitsTwoImagesThatEachNeedTheirFourthLadderCandidate() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val sources = (0..1).map { index ->
            File(root, "$index.jpg").apply { writeBytes(ByteArray(100_000)) }
        }
        var encodes = 0
        val fitter = MmsImageFitter(root) { _, edge, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(if (edge <= 800) 100 else 200)) }
        }

        val result = fitter.fit(
            "two-images", "fingerprint", 200,
            sources.mapIndexed { index, source -> MmsFitPart(source, "image/jpeg", "$index.jpg") },
        ) { parts -> parts.sumOf { it.file.length() } }

        assertTrue(result is MmsFitResult.Fitted)
        assertEquals("each image needs four ladder candidates", 8, encodes)
    }

    @Test
    fun cacheUsesDurableAttachmentIdentityAndReconstructsUnchangedCurrentParts() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        var encodes = 0
        val fitter = MmsImageFitter(root) { _, _, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(100)) }
        }
        fun parts(directory: File) = listOf(
            MmsFitPart(File(directory, "photo.jpg").apply { writeBytes(ByteArray(100_000)) }, "image/jpeg", "photo.jpg", "photo-id"),
            MmsFitPart(File(directory, "sticker.gif").apply { writeBytes(ByteArray(20)) }, "image/gif", "sticker.gif", "sticker-id"),
        )
        val first = parts(File(root, "first").apply { mkdirs() })
        val firstFingerprint = MmsImageFitter.fingerprint(first, MmsLimits(300, 20, true, MmsLimits.Source.FALLBACK), "to|caption")
        fitter.fit("command", firstFingerprint, 300, first) { it.sumOf { part -> part.file.length() } }
        val second = parts(File(root, "fresh-plaintext").apply { mkdirs() })
        val secondFingerprint = MmsImageFitter.fingerprint(second, MmsLimits(300, 20, true, MmsLimits.Source.FALLBACK), "to|caption")
        assertEquals(firstFingerprint, secondFingerprint)
        val result = fitter.fit("command", secondFingerprint, 300, second) { it.sumOf { part -> part.file.length() } }

        assertEquals(1, encodes)
        val fitted = result as MmsFitResult.Fitted
        assertTrue(fitted.parts[0].file.parentFile?.name == "command")
        assertEquals(second[1].file, fitted.parts[1].file)
    }

    @Test
    fun measurementAndPartialLadderFailuresAreTransientUntilOversizeIsProven() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, edge, _, destination ->
            if (edge == 1600) error("encoder unavailable")
            destination.apply { writeBytes(ByteArray(100)) }
        }
        val parts = listOf(MmsFitPart(source, "image/jpeg", "photo.jpg", "photo-id"))

        assertTrue(fitter.fit("measure", "measure", 300, parts) { error("PDU service unavailable") } is MmsFitResult.Transient)
        assertTrue(fitter.fit("partial", "partial", 300, parts) { it.sumOf { part -> part.file.length() } } is MmsFitResult.Fitted)
    }

    @Test
    fun partialEncoderFailureIsTransientWhenTheRemainingCandidateIsStillOversized() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val source = File(root, "source.jpg").apply { writeBytes(ByteArray(100_000)) }
        val fitter = MmsImageFitter(root) { _, edge, _, destination ->
            if (edge < 1600) error("encoder unavailable")
            destination.apply { writeBytes(ByteArray(400)) }
        }

        val result = fitter.fit("partial-oversize", "fingerprint", 300, listOf(MmsFitPart(source, "image/jpeg", "photo.jpg", "photo-id"))) {
            it.sumOf { part -> part.file.length() }
        }

        assertTrue(result is MmsFitResult.Transient)
    }

    @Test
    fun truncatedMetadataRefitsEveryPartInsteadOfOmittingAnAttachment() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        var encodes = 0
        val fitter = MmsImageFitter(root) { _, _, _, destination ->
            encodes++
            destination.apply { writeBytes(ByteArray(100)) }
        }
        val parts = (0..1).map { index ->
            MmsFitPart(File(root, "$index.jpg").apply { writeBytes(ByteArray(100_000)) }, "image/jpeg", "$index.jpg", "photo-$index")
        }
        val measure: (List<MmsFitPart>) -> Long = { it.sumOf { part -> part.file.length() } }
        assertTrue(fitter.fit("truncated", "fingerprint", 300, parts, measure) is MmsFitResult.Fitted)
        File(root, "truncated/fit").writeText("fingerprint\n0\t0-1600-82.jpg\timage/jpeg\t0.jpg\n")
        val beforeRefit = encodes

        val result = fitter.fit("truncated", "fingerprint", 300, parts, measure)

        assertTrue(encodes > beforeRefit)
        assertEquals(2, (result as MmsFitResult.Fitted).parts.size)
    }

    @Test
    fun resizesSmallImagesWhenCaptionMakesTheMessageTooLarge() {
        val root = kotlin.io.path.createTempDirectory("mms-fit").toFile()
        val fitter = MmsImageFitter(root) { _, _, _, destination -> destination.apply { writeBytes(ByteArray(40_000)) } }
        val images = (1..3).map { index ->
            MmsFitPart(File(root, "$index.jpg").apply { writeBytes(ByteArray(85 * 1024)) }, "image/jpeg", "$index.jpg", "photo-$index")
        }

        val result = fitter.fit("caption", "caption", 300L * 1024, images) { parts ->
            60L * 1024 + parts.sumOf { it.file.length() }
        }

        assertTrue("caption plus three 85 KiB JPEGs can use the ladder", result is MmsFitResult.Fitted)
    }

    @Test
    fun honorsEveryExifOrientationForAnAsymmetricImage() {
        val source = Bitmap.createBitmap(2, 3, Bitmap.Config.ARGB_8888)
        (0 until 3).forEach { y -> (0 until 2).forEach { x -> source.setPixel(x, y, 0xff000000.toInt() + y * 2 + x) } }
        val fitter = MmsImageFitter(kotlin.io.path.createTempDirectory("mms-fit").toFile())
        assertTrue(fitter.orient(source, ExifInterface.ORIENTATION_NORMAL) === source)
        (ExifInterface.ORIENTATION_FLIP_HORIZONTAL..ExifInterface.ORIENTATION_ROTATE_270).forEach { orientation ->
            val matrix = fitter.orientationMatrix(orientation)
            assertTrue("orientation $orientation has a transform", matrix != null && !matrix.isIdentity)
        }
        source.recycle()
    }
}
