package dev.peppy.mobile

import androidx.test.core.app.ApplicationProvider
import androidx.core.content.FileProvider
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Before
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class DiagnosticsLogTest {
    private val context = ApplicationProvider.getApplicationContext<android.content.Context>()
    private val retainedDirectory = File(context.noBackupFilesDir, "diagnostics")
    private val shareDirectory = File(context.cacheDir, "diagnostics-share")

    @Before fun resetFileProviderCache() {
        val cacheField = FileProvider::class.java.getDeclaredField("sCache").apply { isAccessible = true }
        val cache = cacheField.get(null) as MutableMap<*, *>
        synchronized(cache) { cache.clear() }
    }

    @After fun cleanUp() {
        retainedDirectory.deleteRecursively()
        shareDirectory.deleteRecursively()
    }

    @Test fun disabledLoggerDoesNotCreateRetainedDirectory() {
        val log = DiagnosticsLog(context) { _, _, _ -> }

        log.setEnabled(false)
        log.record(record())

        assertFalse(retainedDirectory.exists())
    }

    @Test fun startupOffPublishesExistingRetainedBytesWithoutCreatingFiles() {
        retainedDirectory.mkdirs()
        File(retainedDirectory, "current.log").writeText("${record()}\n")
        val published = CountDownLatch(1)
        var bytes = 0L
        val log = DiagnosticsLog(context) { size, _, _ -> bytes = size; published.countDown() }

        log.initialize()

        assertTrue(published.await(2, TimeUnit.SECONDS))
        assertEquals(File(retainedDirectory, "current.log").length(), bytes)
    }

    @Test fun cleanStartupDoesNotRewriteBoundedCompleteRetainedFiles() {
        retainedDirectory.mkdirs()
        val current = File(retainedDirectory, "current.log")
        val content = "${record()}\n"
        current.writeText(content)
        val originalModified = 1_700_000_000_000L
        assertTrue(current.setLastModified(originalModified))
        val published = CountDownLatch(1)
        val log = DiagnosticsLog(context) { _, _, _ -> published.countDown() }

        log.initialize()

        assertTrue(published.await(2, TimeUnit.SECONDS))
        assertEquals(content, current.readText())
        assertEquals(originalModified, current.lastModified())
    }

    @Test fun nearBoundaryBatchRotatesWithoutDeletingRetainedHistory() = runBlocking {
        retainedDirectory.mkdirs()
        val old = record(1, " 30=111")
        File(retainedDirectory, "current.log").writeText(("$old\n").repeat(2 * 1024 * 1024 / (old.length + 1)))
        val log = DiagnosticsLog(context) { _, _, _ -> }
        log.setEnabled(true)
        log.record(record(2, " 30=222"))

        log.export()

        val files = retainedDirectory.listFiles().orEmpty()
        assertTrue(files.size <= 2)
        assertTrue(files.all { it.length() <= 2L * 1024 * 1024 })
        assertTrue(File(retainedDirectory, "previous.log").readText().contains(old))
    }

    @Test fun startupRepairKeepsNewestCompleteRecordsWithinFourMiB() = runBlocking {
        retainedDirectory.mkdirs()
        writeOversize(File(retainedDirectory, "previous.log"), 1)
        writeOversize(File(retainedDirectory, "current.log"), 2)
        val log = DiagnosticsLog(context, { file -> android.net.Uri.fromFile(file) }) { _, _, _ -> }

        val uri = log.export()

        assertNotNull(uri)
        assertTrue(retainedDirectory.listFiles().orEmpty().sumOf(File::length) <= 4L * 1024 * 1024)
        assertTrue(File(retainedDirectory, "current.log").readText().contains(" 30=24499"))
    }

    @Test fun validLookingUnterminatedTailIsDiscardedInsteadOfJoinedToNextRecord() = runBlocking {
        retainedDirectory.mkdirs()
        File(retainedDirectory, "current.log").writeText("${record(1)}\n${record(2)}")
        val log = DiagnosticsLog(context) { _, _, _ -> }

        log.export()

        val repaired = File(retainedDirectory, "current.log").readLines()
        assertEquals(listOf(record(1)), repaired)
    }

    @Test fun exportFlushesMoreThanOneWriterBatchAndUsesInjectedUniqueUris() = runBlocking {
        val snapshots = mutableListOf<File>()
        val log = DiagnosticsLog(context, { file -> snapshots += file; android.net.Uri.fromFile(file) }) { _, _, _ -> }
        log.setEnabled(true)
        repeat(128) { log.record(record(1, " 30=$it")) }

        val first = log.export()
        val second = log.export()

        assertNotNull(first)
        assertNotNull(second)
        assertEquals(2, snapshots.size)
        assertFalse(snapshots[0].name == snapshots[1].name)
        assertTrue(snapshots[1].readLines().count { it.startsWith("event=") } >= 128)
        assertTrue(snapshots[1].readLines().any { it.startsWith("event=24 ") && it.contains(" 34=") && it.contains(" 35=") })
    }

    @Test fun exportHeaderDocumentsTheAdditiveMmsCapturePassSchema() = runBlocking {
        var snapshot: File? = null
        val log = DiagnosticsLog(context, { file -> snapshot = file; android.net.Uri.fromFile(file) }) { _, _, _ -> }
        log.setEnabled(true)

        assertNotNull(log.export())

        val lines = requireNotNull(snapshot).readLines()
        val header = lines.first()
        assertTrue(header.startsWith("schema=2 "))
        assertTrue(header.contains("25:mms_capture_pass"))
        assertTrue(header.contains("40:provider_reads"))
        assertTrue(header.contains("15:mms_recheck_selection_unclassified"))
        assertTrue(header.contains("9:locked gateway_progress phase=20:mms_cleanup"))
        assertTrue(header.contains("count=marked,processed=unlinked,pending=pending_unlink"))
        assertTrue(lines[1].startsWith("event=24 "))
        assertFalse(lines.any { it.isBlank() })
    }

    @Test fun fullQueueDropsRecordsWithoutBlockingTheProducer() = runBlocking {
        var dropped = 0L
        val writerEntered = CountDownLatch(1)
        val releaseWriter = CountDownLatch(1)
        val log = DiagnosticsLog(context, { file ->
            writerEntered.countDown()
            check(releaseWriter.await(30, TimeUnit.SECONDS))
            android.net.Uri.fromFile(file)
        }) { _, nextDropped, _ -> dropped = nextDropped }
        log.setEnabled(true)
        val executor = Executors.newFixedThreadPool(2)
        val exported = executor.submit<android.net.Uri?> { runBlocking { log.export() } }
        var produced: java.util.concurrent.Future<*>? = null
        var testFailure: Throwable? = null

        try {
            assertTrue(writerEntered.await(10, TimeUnit.SECONDS))
            produced = executor.submit {
                repeat(4_096) { log.record(record(1, " 30=$it")) }
            }
            produced.get(10, TimeUnit.SECONDS)
            releaseWriter.countDown()

            assertNotNull(exported.get(10, TimeUnit.SECONDS))
            assertNotNull(log.export())
            assertTrue(dropped > 0)
        } catch (failure: Throwable) {
            testFailure = failure
            throw failure
        } finally {
            log.setEnabled(false)
            releaseWriter.countDown()
            var cleanupFailure: Throwable? = null
            var executorTerminated = false
            try {
                executor.shutdown()
                executorTerminated = executor.awaitTermination(10, TimeUnit.SECONDS)
                if (!executorTerminated) {
                    executor.shutdownNow()
                    executorTerminated = executor.awaitTermination(10, TimeUnit.SECONDS)
                    assertTrue(executorTerminated)
                }
                if (executorTerminated) {
                    produced?.get()
                    exported.get()
                }
            } catch (failure: Throwable) {
                cleanupFailure = failure
            } finally {
                if (!executor.isTerminated) executor.shutdownNow()
            }
            if (executorTerminated) {
                try {
                    log.export()
                } catch (failure: Throwable) {
                    if (cleanupFailure == null) cleanupFailure = failure else cleanupFailure.addSuppressed(failure)
                }
            }
            cleanupFailure?.let { failure ->
                testFailure?.addSuppressed(failure) ?: throw failure
            }
        }
    }

    @Test fun providerFactoryFailureCleansPartialSnapshotAndReportsTypedError() = runBlocking {
        var error: DiagnosticsError? = null
        val log = DiagnosticsLog(context, { throw IllegalStateException("fixture failure") }) { _, _, nextError -> error = nextError }
        log.setEnabled(true)
        log.record(record())

        val exported = log.export()

        assertNull(exported)
        assertEquals(DiagnosticsError.EXPORT, error)
        assertTrue(shareDirectory.listFiles().orEmpty().isEmpty())
    }

    @Test fun storageAndRotationFailuresAreReportedWithoutEscapingProducer() {
        retainedDirectory.parentFile!!.mkdirs()
        retainedDirectory.writeText("not a directory")
        val storageError = CountDownLatch(1)
        val log = DiagnosticsLog(context) { _, _, error -> if (error == DiagnosticsError.STORAGE) storageError.countDown() }

        log.setEnabled(true)
        log.record(record())

        assertTrue(storageError.await(2, TimeUnit.SECONDS))
        assertTrue(retainedDirectory.isFile)
    }

    @Test fun rotationDeleteFailureIsReportedWithoutDeletingHistory() = runBlocking {
        retainedDirectory.mkdirs()
        val old = record(1, " 30=111")
        File(retainedDirectory, "current.log").writeText(("$old\n").repeat(2 * 1024 * 1024 / (old.length + 1)))
        val blockedPrevious = File(retainedDirectory, "previous.log").apply { mkdirs() }
        File(blockedPrevious, "keep").writeText("history")
        val storageError = CountDownLatch(1)
        val log = DiagnosticsLog(context, { file -> android.net.Uri.fromFile(file) }) { _, _, error ->
            if (error == DiagnosticsError.STORAGE) storageError.countDown()
        }
        log.setEnabled(true)
        log.record(record(2, " 30=222"))

        log.export()

        assertTrue(storageError.await(2, TimeUnit.SECONDS))
        assertTrue(File(blockedPrevious, "keep").exists())
    }

    @Test fun clearReportsFailedDeletesAndRemainingBytes() = runBlocking {
        shareDirectory.mkdirs()
        val undeletable = File(shareDirectory, "nested").apply { mkdirs() }
        File(undeletable, "snapshot.log").writeText("held")
        var error: DiagnosticsError? = null
        val log = DiagnosticsLog(context) { _, _, nextError -> error = nextError }

        log.clear()

        assertEquals(DiagnosticsError.CLEAR, error)
        assertTrue(undeletable.exists())
    }

    @Test fun clearFencesQueuedDataAndDeletesGrantedSnapshots() = runBlocking {
        val log = DiagnosticsLog(context, { file -> android.net.Uri.fromFile(file) }) { _, _, _ -> }
        log.setEnabled(true)
        repeat(128) { log.record(record(1, " 30=$it")) }
        assertNotNull(log.export())

        log.clear()

        assertTrue(retainedDirectory.listFiles().orEmpty().isEmpty())
        assertTrue(shareDirectory.listFiles().orEmpty().isEmpty())
    }

    @Test fun actualFileProviderIntegrationProducesContentUri() = runBlocking {
        shareDirectory.mkdirs()
        val fixture = File(shareDirectory, "provider-fixture.log").apply { writeText("fixture") }
        val directUri = FileProvider.getUriForFile(context, "${BuildConfig.APPLICATION_ID}.mms-files", fixture)
        val log = DiagnosticsLog(context) { _, _, _ -> }
        log.setEnabled(true)
        log.record(record())

        val uri = log.export()

        assertNotNull(uri)
        assertEquals("content", directUri.scheme)
        assertEquals("content", uri!!.scheme)
        assertTrue(uri.toString().contains("mms-files"))
    }

    private fun writeOversize(file: File, event: Int) {
        val payload = "9".repeat(430)
        file.bufferedWriter().use { output ->
            repeat(4_500) { index -> output.appendLine(record(event, " 30=$event$index 31=$payload")) }
        }
    }

    private fun record(event: Int = 1, fields: String = ""): String = "event=$event session=1 timestamp_ms=1 trace=1$fields"
}
