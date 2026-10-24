package dev.peppy.mobile

import android.content.Context
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.File
import java.io.FileOutputStream
import java.nio.charset.StandardCharsets
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

internal class DiagnosticsLog(
    private val context: Context,
    private val uriFactory: (File) -> Uri = { file ->
        FileProvider.getUriForFile(context, "${BuildConfig.APPLICATION_ID}.mms-files", file)
    },
    private val onState: (Long, Long, DiagnosticsError?) -> Unit,
) {
    private data class Record(val generation: Long, val sequence: Long, val text: String)

    private val records = ArrayBlockingQueue<Record>(QUEUE_CAPACITY)
    private val dropped = AtomicLong()
    private val accepted = AtomicLong()
    private val enabled = AtomicBoolean(false)
    private val clearing = AtomicBoolean(false)
    private val generation = AtomicLong()
    private val drainScheduled = AtomicBoolean(false)
    private val startupScheduled = AtomicBoolean(false)
    private val owner = ThreadPoolExecutor(
        1, 1, IDLE_TIMEOUT_SECONDS, TimeUnit.SECONDS,
        ArrayBlockingQueue(CONTROL_QUEUE_CAPACITY),
        { runnable -> Thread(runnable, "peppy-diagnostics-writer").apply { isDaemon = true } },
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val directory = File(context.noBackupFilesDir, "diagnostics")
    private val shareDirectory = File(context.cacheDir, "diagnostics-share")

    // Owner-thread state. File.length() is never used while a buffered stream is open.
    private var retainedSize = 0L
    private var currentSize = 0L
    private var previousSize = 0L
    private var repaired = false

    fun initialize() {
        if (startupScheduled.compareAndSet(false, true)) {
            submitOwner {
                try {
                    repairRetainedFiles()
                    publish(null)
                } catch (_: Exception) {
                    handleStorageFailure()
                }
            }
        }
    }

    fun setEnabled(value: Boolean) {
        val nextGeneration = generation.incrementAndGet()
        enabled.set(value)
        if (!value) records.removeIf { it.generation < nextGeneration }
        if (value && records.isNotEmpty()) scheduleDrain()
    }

    fun record(line: String) {
        val recordGeneration = generation.get()
        if (!enabled.get() || clearing.get()) return
        val bytes = line.toByteArray(StandardCharsets.UTF_8)
        if (!RECORD_PATTERN.matches(line) || bytes.size + NEWLINE_BYTES > MAX_RECORD_BYTES) {
            dropped.incrementAndGet()
            return
        }
        val record = Record(recordGeneration, accepted.incrementAndGet(), line)
        if (!enabled.get() || generation.get() != recordGeneration || clearing.get() || !records.offer(record)) {
            dropped.incrementAndGet()
            return
        }
        scheduleDrain(record)
    }

    suspend fun export(): Uri? = onOwner {
        try {
            repairRetainedFiles()
            val barrier = accepted.get()
            drainThrough(generation.get(), barrier)
            createSnapshot()
        } catch (_: Exception) {
            publish(DiagnosticsError.EXPORT)
            null
        }
    }

    suspend fun clear() {
        clearing.set(true)
        generation.incrementAndGet()
        records.clear()
        try {
            onOwner {
                var failed = false
                failed = deleteContents(directory) || failed
                failed = deleteContents(shareDirectory) || failed
                previousSize = File(directory, PREVIOUS_FILE).takeIf(File::isFile)?.length() ?: 0
                currentSize = currentFile().takeIf(File::isFile)?.length() ?: 0
                retainedSize = previousSize + currentSize
                repaired = true
                publish(if (failed) DiagnosticsError.CLEAR else null)
            }
        } finally {
            clearing.set(false)
        }
    }

    private fun scheduleDrain(record: Record? = null) {
        if (!drainScheduled.compareAndSet(false, true)) return
        if (!submitOwner {
                try {
                    repairRetainedFiles()
                    drainThrough(generation.get(), Long.MAX_VALUE, BATCH_SIZE)
                } catch (_: Exception) {
                    handleStorageFailure()
                } finally {
                    drainScheduled.set(false)
                    if (records.isNotEmpty() && enabled.get() && !clearing.get()) scheduleDrain()
                }
            }) {
            drainScheduled.set(false)
            if (record != null && records.remove(record)) dropped.incrementAndGet()
        }
    }

    private fun drainThrough(activeGeneration: Long, barrier: Long, limit: Int = QUEUE_CAPACITY) {
        val batch = ArrayList<String>(BATCH_SIZE)
        var drained = 0
        while (drained < limit) {
            if ((records.peek() ?: break).sequence > barrier) break
            val record = records.poll() ?: continue
            drained += 1
            if (record.sequence <= barrier && record.generation == activeGeneration && generation.get() == activeGeneration && enabled.get() && !clearing.get()) {
                batch += record.text
                if (batch.size == BATCH_SIZE) {
                    appendBatch(batch)
                    batch.clear()
                }
            }
        }
        if (batch.isNotEmpty()) appendBatch(batch)
        if (drained > 0) publish(null)
    }

    private fun appendBatch(batch: List<String>) {
        if (!directory.exists() && !directory.mkdirs()) throw IllegalStateException("diagnostics directory unavailable")
        var output: BufferedOutputStream? = null
        try {
            batch.forEach { line ->
                val bytes = line.toByteArray(StandardCharsets.UTF_8)
                if (currentSize + bytes.size + NEWLINE_BYTES > MAX_FILE_BYTES) {
                    output?.close()
                    output = null
                    rotate()
                }
                if (output == null) output = BufferedOutputStream(FileOutputStream(currentFile(), true))
                output.write(bytes)
                output.write(NEWLINE)
                currentSize += bytes.size + NEWLINE_BYTES
                retainedSize += bytes.size + NEWLINE_BYTES
            }
        } finally {
            output?.close()
        }
    }

    /** Repairs retained files once on the owner, without creating a directory when logging is off. */
    private fun repairRetainedFiles() {
        if (repaired) return
        repaired = true
        if (!directory.isDirectory) {
            retainedSize = 0
            return
        }
        previousSize = repairFile(File(directory, PREVIOUS_FILE))
        currentSize = repairFile(currentFile())
        retainedSize = previousSize + currentSize
    }

    private fun repairFile(file: File): Long {
        if (!file.isFile) return 0
        val records = ArrayDeque<ByteArray>()
        var total = 0L
        var repairNeeded = file.length() > MAX_FILE_BYTES
        BufferedInputStream(file.inputStream()).use { input ->
            val line = ArrayList<Byte>(MAX_RECORD_BYTES)
            var oversized = false
            while (true) {
                when (val value = input.read()) {
                    -1 -> break // A non-newline-terminated tail is deliberately discarded.
                    NEWLINE[0].toInt() -> {
                        if (!oversized) {
                            val bytes = line.toByteArray()
                            val text = String(bytes, StandardCharsets.UTF_8)
                            if (bytes.size + NEWLINE_BYTES <= MAX_RECORD_BYTES && RECORD_PATTERN.matches(text)) {
                                while (total + bytes.size + NEWLINE_BYTES > MAX_FILE_BYTES && records.isNotEmpty()) {
                                    total -= records.removeFirst().size + NEWLINE_BYTES
                                    repairNeeded = true
                                }
                                if (bytes.size + NEWLINE_BYTES <= MAX_FILE_BYTES) {
                                    records.addLast(bytes)
                                    total += bytes.size + NEWLINE_BYTES
                                }
                            } else repairNeeded = true
                        } else {
                            repairNeeded = true
                        }
                        line.clear()
                        oversized = false
                    }
                    else -> if (line.size < MAX_RECORD_BYTES) line += value.toByte() else oversized = true
                }
            }
            if (line.isNotEmpty() || oversized) repairNeeded = true
        }
        if (repairNeeded) {
            FileOutputStream(file, false).buffered().use { output ->
                records.forEach { bytes -> output.write(bytes); output.write(NEWLINE) }
            }
        }
        return total
    }

    private fun rotate() {
        val previous = File(directory, PREVIOUS_FILE)
        if (previous.exists() && !previous.delete()) {
            refreshRetainedSizes()
            throw IllegalStateException("unable to rotate diagnostics")
        }
        previousSize = 0
        val current = currentFile()
        if (current.exists() && !current.renameTo(previous)) {
            refreshRetainedSizes()
            throw IllegalStateException("unable to rotate diagnostics")
        }
        previousSize = currentSize
        currentSize = 0
        retainedSize = previousSize
    }

    private fun createSnapshot(): Uri? {
        var snapshot: File? = null
        return try {
            if (!shareDirectory.exists() && !shareDirectory.mkdirs()) throw IllegalStateException("share directory unavailable")
            if (shareDirectory.listFiles()?.any { !it.delete() } == true) throw IllegalStateException("unable to clear prior snapshots")
            val outputFile = File(shareDirectory, "diagnostics-${UUID.randomUUID()}.log")
            snapshot = outputFile
            var remaining = MAX_TOTAL_BYTES
            BufferedOutputStream(FileOutputStream(outputFile)).use { output ->
                remaining -= write(output, HEADER.toByteArray(StandardCharsets.UTF_8), remaining)
                remaining -= write(output, NEWLINE, remaining)
                remaining -= write(output, stateRecord().toByteArray(StandardCharsets.UTF_8), remaining)
                remaining -= write(output, NEWLINE, remaining)
                copyNewestRecords(output, remaining)
            }
            uriFactory(outputFile)
        } catch (_: Exception) {
            snapshot?.delete()
            publish(DiagnosticsError.EXPORT)
            null
        }
    }

    private fun copyNewestRecords(output: BufferedOutputStream, initialRemaining: Long) {
        val all = ArrayDeque<ByteArray>()
        retainedFiles().forEach { file ->
            file.forEachLine { line ->
                val bytes = line.toByteArray(StandardCharsets.UTF_8)
                if (RECORD_PATTERN.matches(line) && bytes.size + NEWLINE_BYTES <= MAX_RECORD_BYTES) all.addLast(bytes)
            }
        }
        var total = all.sumOf { it.size + NEWLINE_BYTES }.toLong()
        while (total > initialRemaining && all.isNotEmpty()) total -= all.removeFirst().size + NEWLINE_BYTES
        all.forEach { bytes -> output.write(bytes); output.write(NEWLINE) }
    }

    private fun deleteContents(folder: File): Boolean {
        if (!folder.exists()) return false
        val files = folder.listFiles() ?: return true
        return files.any { !it.delete() }
    }

    private fun retainedFiles(): List<File> = listOf(File(directory, PREVIOUS_FILE), currentFile()).filter(File::isFile)
    private fun currentFile(): File = File(directory, CURRENT_FILE)
    private fun publish(error: DiagnosticsError?) = onState(retainedSize, dropped.get(), error)
    private fun stateRecord(): String = "event=24 session=0 timestamp_ms=0 trace=0 34=${dropped.get()} 35=${if (enabled.get()) 1 else 0}"
    private fun handleStorageFailure() {
        repaired = false
        refreshRetainedSizes()
        publish(DiagnosticsError.STORAGE)
    }
    private fun refreshRetainedSizes() {
        previousSize = File(directory, PREVIOUS_FILE).takeIf(File::isFile)?.length() ?: 0
        currentSize = currentFile().takeIf(File::isFile)?.length() ?: 0
        retainedSize = previousSize + currentSize
    }
    private fun write(output: BufferedOutputStream, bytes: ByteArray, remaining: Long): Int {
        val count = minOf(bytes.size.toLong(), remaining).toInt()
        output.write(bytes, 0, count)
        return count
    }

    private fun submitOwner(task: () -> Unit): Boolean = try {
        owner.execute(task)
        true
    } catch (_: RejectedExecutionException) {
        false
    }

    private suspend fun <T> onOwner(block: () -> T): T = suspendCancellableCoroutine { continuation ->
        if (!submitOwner { runCatching(block).onSuccess(continuation::resume).onFailure(continuation::resumeWithException) }) {
            continuation.resumeWithException(RejectedExecutionException("diagnostics writer unavailable"))
        }
    }

    private companion object {
        const val QUEUE_CAPACITY = 256
        const val CONTROL_QUEUE_CAPACITY = 8
        const val BATCH_SIZE = 64
        const val MAX_RECORD_BYTES = 512
        const val MAX_FILE_BYTES = 2 * 1024 * 1024L
        const val MAX_TOTAL_BYTES = 4 * 1024 * 1024L
        const val IDLE_TIMEOUT_SECONDS = 1L
        const val CURRENT_FILE = "current.log"
        const val PREVIOUS_FILE = "previous.log"
        const val NEWLINE_BYTES = 1
        val NEWLINE = byteArrayOf('\n'.code.toByte())
        val RECORD_PATTERN = Regex("event=\\d+ session=\\d+ timestamp_ms=\\d+ trace=\\d+(?: \\d+=-?\\d+)*")
        val HEADER = "schema=1 version=${BuildConfig.VERSION_NAME}/${BuildConfig.VERSION_CODE} source=${BuildConfig.PEPPY_SOURCE_COMMIT} dirty=${BuildConfig.PEPPY_SOURCE_DIRTY_STATE} diff_sha256=${BuildConfig.PEPPY_SOURCE_DIFF_SHA256} nativeProfile=${BuildConfig.PEPPY_NATIVE_PROFILE} events=1:observer_hint,2:observer_dispatch,3:capture_enqueue,4:capture_start,5:capture_end,6:capture_retry,7:capture_continuation,8:capture_plan,9:capture_summary,10:provider_query,11:native_open,12:gateway_enqueue,13:gateway_start,14:gateway_end,15:gateway_phase,16:http_request,17:outbox_ack,18:capture_progress,19:native_call,20:gateway_progress(rows=live_received,pending=snapshot_received,processed=applied_quarantine_drained_superseded,count=uploaded,remaining=snapshot_remaining,ready=contacts_ready,backfill_pending=local_backfill_pending),21:contact_progress(more=more_contact_work),22:socket_state(rows=ready_frames,captured=event_frames,processed=resync_frames,pending=other_frames),23:notification_progress(rows=active,processed=reconciled,count=dismissed),24:diagnostic_state fields=1:trace_id,2:parent_id,3:enqueued_ms,4:delay_ms,5:attempt,6:duration_ms,7:cpu_ms,8:success,9:source,10:captured,11:pending,12:unavailable,13:paused,14:more,15:provider_error,16:core_error,17:imported,18:rows,19:count,20:phase,21:method,22:route,23:status,24:request_bytes,25:response_bytes,26:cold,28:outcome,29:reason,30:processed,31:advanced,32:wrapped,33:budget_exhausted,34:dropped,35:enabled,36:stage,37:remaining,38:backfill_pending,39:ready stages=1:sms_live,2:sms_history,3:sms_recheck,4:sms_reconciliation,5:mms,6:gateway,7:contact_full,8:contact_incremental,9:socket,10:notification reasons=0:unknown,1:more,2:hint,3:budget,4:provider_error,5:core_error,6:complete,7:connected,8:reconnecting,9:locked"
    }
}
