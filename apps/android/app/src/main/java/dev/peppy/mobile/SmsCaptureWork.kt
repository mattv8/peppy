package dev.peppy.mobile

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.CoroutineWorker
import androidx.work.Data
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.annotation.VisibleForTesting
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class SmsAcquisitionState { PENDING, UNAVAILABLE, COMPLETE, PAUSED }

data class SmsAcquisitionInput(
    val sourceGeneration: String,
    val subscriptionId: String,
    val providerMessageId: String,
    val providerThreadId: String?,
    val incoming: Boolean,
    val senderAddress: String?,
    val recipients: List<String>,
    val body: String,
    val imported: Boolean,
    val sentAtMs: Long?,
    val dateSentMs: Long?,
    val creator: String?,
    val sourceRowFingerprint: String = "",
    /** The `SMS_RECEIVED` identity derived from this row's evidence, for incoming rows only. */
    val broadcastId: String? = null,
)

data class SmsAcquisition(val id: String, val input: SmsAcquisitionInput, val state: SmsAcquisitionState, val reason: String?)

/** The core owns persisted scan state, including frozen history baselines and capacity pausing. */
internal data class SmsCapturePlan(
    val sourceGeneration: String,
    val imported: Boolean,
    val checkpoint: Long,
    val subscriptionId: String = SmsCapture.PROVIDER_SCOPE,
    val pendingRecheckCursor: String? = null,
    val historyUpperBound: Long? = null,
    val historyComplete: Boolean = false,
    val paused: Boolean = false,
)

/** Core-owned frozen sweep state for IDs at or below forward scan checkpoints. */
internal data class SmsReconciliationPlan(
    val sourceGeneration: String,
    val subscriptionId: String,
    val cursor: Long,
    val upperBound: Long,
    /** Rows at or below the frozen history baseline are imported history (read, no banner). */
    val historyUpperBound: Long? = null,
)

internal data class SmsReconciliationResult(
    val summary: SmsCaptureSummary,
    val cursor: Long,
    val exhausted: Boolean,
)

internal enum class SmsCompletion {
    /** A new message was published. */
    CAPTURED,
    /** The row reconciled to an existing message. */
    DUPLICATE,
    /** The core made the row terminal (for example an ambiguous reconciliation); never retried. */
    UNAVAILABLE,
}

/** The core cannot accept a new row now: its pending queue is full or history is paused. */
internal class SmsCaptureCapacityException : IllegalStateException("SMS acquisition capacity reached")

internal interface SmsCaptureCore {
    /** Rust returns a durable, rotated page of pending rows after [afterId]. */
    fun acquisitions(afterId: String?, limit: Int): List<SmsAcquisition>
    /**
     * Durably records a provider row. Unrepresentable content returns an UNAVAILABLE acquisition;
     * [SmsCaptureCapacityException] means backpressure; any other exception is a core failure.
     */
    fun begin(input: SmsAcquisitionInput): SmsAcquisition
    /** Publishes a pending acquisition. */
    fun complete(id: String): SmsCompletion
    fun setState(id: String, state: SmsAcquisitionState, reason: String?)
    fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String)
}

internal interface SmsCaptureProvider {
    fun page(afterId: Long, limit: Int): SmsProviderPage
    fun read(id: Long): SmsProviderRecord?
}

data class SmsCaptureSummary(
    val captured: Int = 0,
    val pending: Int = 0,
    val unavailable: Int = 0,
    val paused: Int = 0,
    val more: Boolean = false,
    val providerError: Boolean = false,
    val pendingRecheckCursor: String? = null,
    /** The frozen history bound was observed drained; the adapter must acknowledge it in core. */
    val historyExhausted: Boolean = false,
    /** The core rejected or failed a call; distinct from Android provider read/permission errors. */
    val coreError: Boolean = false,
)

/** Host-owned binding from scheduled Android work to the generated core adapter. */
internal fun interface SmsCaptureRunner {
    fun run(context: Context): SmsCaptureSummary
}

/** Offline provider scheduling; a missing generated adapter retries rather than claiming capture succeeded. */
object SmsCaptureWork {
    const val NAME = "peppy-sms-capture"
    const val PERIODIC_NAME = "peppy-sms-capture-periodic"

    @Volatile internal var runner: SmsCaptureRunner? = null
    @VisibleForTesting
    @Volatile internal var fastModeOverride: Boolean? = null
    private val executionMutex = Mutex()
    /** A hint that arrived while a pass may already be past the provider rows it changed. */
    private val hintCount = java.util.concurrent.atomic.AtomicInteger(0)

    /** Coalesced hint: KEEP drops it while a pass runs, so the running pass re-checks [hintPending]. */
    fun enqueue(context: Context) {
        hintCount.incrementAndGet()
        schedule(context, ExistingWorkPolicy.KEEP, SmsCaptureRequestSource.DEFAULT)
    }

    /** Provider-observer path only: fast debug builds request immediate unique work. */
    internal fun enqueueObserver(context: Context) {
        hintCount.incrementAndGet()
        schedule(context, ExistingWorkPolicy.KEEP, SmsCaptureRequestSource.OBSERVER)
    }

    internal fun takeHint(): Boolean = takeHintCount() > 0

    internal fun takeHintCount(): Int = hintCount.getAndSet(0)

    /** Append a bounded pass behind the active worker: KEEP would discard this continuation. */
    internal fun enqueueContinuation(context: Context) =
        schedule(context, ExistingWorkPolicy.APPEND_OR_REPLACE, SmsCaptureRequestSource.CONTINUATION)

    internal suspend fun <T> serialized(block: suspend () -> T): T = executionMutex.withLock { block() }

    fun ensurePeriodic(context: Context) {
        WorkManager.getInstance(context).enqueueUniquePeriodicWork(
            PERIODIC_NAME,
            ExistingPeriodicWorkPolicy.KEEP,
            PeriodicWorkRequestBuilder<SmsCaptureWorker>(15, TimeUnit.MINUTES)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build(),
        )
    }

    private fun schedule(context: Context, policy: ExistingWorkPolicy, source: SmsCaptureRequestSource) {
        val fastMode = fastModeOverride ?: BuildConfig.PEPPY_SMS_FAST_CAPTURE
        val delayMs = smsCaptureInitialDelayMs(source, fastMode)
        val diagnostic = captureDiagnosticRequest(source, delayMs, fastMode)
        val request = createRequest(source, fastMode, diagnostic?.data())
        WorkManager.getInstance(context).enqueueUniqueWork(NAME, policy, request)
    }

    internal fun createRequest(source: SmsCaptureRequestSource, fastMode: Boolean, diagnosticData: Data? = null) =
        OneTimeWorkRequestBuilder<SmsCaptureWorker>()
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
            .apply { smsCaptureInitialDelayMs(source, fastMode).takeIf { it > 0 }?.let { setInitialDelay(it, TimeUnit.MILLISECONDS) } }
            .apply { if (diagnosticData != null) setInputData(diagnosticData) }
            .build()

    private fun captureDiagnosticRequest(source: SmsCaptureRequestSource, delayMs: Long, fastMode: Boolean): CaptureDiagnosticRequest? {
        if (!SyncDiagnostics.enabled) return null
        val parent = SyncDiagnostics.currentTraceId()
        val traceId = SyncDiagnostics.newTraceId()
        val enqueuedMs = SyncDiagnostics.nowMs()
        SyncDiagnostics.withTrace(traceId) {
            SyncDiagnostics.record(
                SyncTraceEvent.CAPTURE_ENQUEUE,
                SyncTraceField.PARENT_ID to parent,
                SyncTraceField.SOURCE to source.code,
                SyncTraceField.DELAY_MS to delayMs,
                SyncTraceField.FAST_MODE to if (fastMode) 1 else 0,
                SyncTraceField.ENQUEUED_MS to enqueuedMs,
            )
        }
        return CaptureDiagnosticRequest(traceId, SyncDiagnostics.currentSessionToken(), enqueuedMs, source.code)
    }

    internal const val DIAGNOSTIC_TRACE_ID = "peppy-sms-capture-trace"
    internal const val DIAGNOSTIC_ENQUEUED_MS = "peppy-sms-capture-enqueued-ms"
    internal const val DIAGNOSTIC_SESSION = "peppy-sms-capture-session"
    internal const val DIAGNOSTIC_SOURCE = "peppy-sms-capture-source"
}

internal enum class SmsCaptureRequestSource(val code: Long) { DEFAULT(0), OBSERVER(1), CONTINUATION(2), UNKNOWN(3) }

internal fun smsCaptureInitialDelayMs(source: SmsCaptureRequestSource, fastMode: Boolean): Long = when (source) {
    SmsCaptureRequestSource.OBSERVER -> if (fastMode) 0 else CAPTURE_DELAY_MS
    SmsCaptureRequestSource.DEFAULT -> CAPTURE_DELAY_MS
    SmsCaptureRequestSource.CONTINUATION -> 0
    SmsCaptureRequestSource.UNKNOWN -> CAPTURE_DELAY_MS
}

private const val CAPTURE_DELAY_MS = 15_000L

private data class CaptureDiagnosticRequest(val traceId: Long, val session: Long, val enqueuedMs: Long, val source: Long) {
    fun data(): Data = Data.Builder()
        .putLong(SmsCaptureWork.DIAGNOSTIC_TRACE_ID, traceId)
        .putLong(SmsCaptureWork.DIAGNOSTIC_SESSION, session)
        .putLong(SmsCaptureWork.DIAGNOSTIC_ENQUEUED_MS, enqueuedMs)
        .putLong(SmsCaptureWork.DIAGNOSTIC_SOURCE, source)
        .build()
}

class SmsCaptureWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        SmsCaptureWork.serialized {
            val consumedHints = SmsCaptureWork.takeHintCount()
            val diagnostic = if (SyncDiagnostics.enabled) captureDiagnosticInput() else null
            val traceId = diagnostic?.traceId ?: if (SyncDiagnostics.enabled) SyncDiagnostics.newTraceId() else 0L
            var succeeded = false
            var outcome = WORK_OUTCOME_THROW
            try {
                val runner = SmsCaptureWork.runner
                if (runner == null) {
                    if (diagnostic != null) SyncDiagnostics.withTrace(traceId) {
                        SyncDiagnostics.record(SyncTraceEvent.CAPTURE_RETRY, SyncTraceField.UNAVAILABLE to 1L)
                    }
                    outcome = WORK_OUTCOME_RETRY
                    return@serialized Result.retry()
                }
                val summary = if (diagnostic != null) {
                    SyncDiagnostics.withTrace(traceId) {
                        SyncDiagnostics.record(
                            SyncTraceEvent.CAPTURE_START,
                            SyncTraceField.ATTEMPT to runAttemptCount.toLong(),
                            SyncTraceField.SOURCE to diagnostic.source,
                            SyncTraceField.ENQUEUED_MS to (diagnostic.enqueuedMs ?: 0L),
                            SyncTraceField.COUNT to consumedHints.toLong(),
                            *(diagnostic.delayFields()),
                        )
                        runner.run(applicationContext)
                    }
                } else {
                    runner.run(applicationContext)
                }
                if (diagnostic != null) {
                    SyncDiagnostics.withTrace(traceId) {
                        SyncDiagnostics.record(
                            SyncTraceEvent.CAPTURE_SUMMARY,
                            SyncTraceField.CAPTURED to summary.captured.toLong(),
                            SyncTraceField.PENDING to summary.pending.toLong(),
                            SyncTraceField.UNAVAILABLE to summary.unavailable.toLong(),
                            SyncTraceField.PAUSED to summary.paused.toLong(),
                            SyncTraceField.MORE to if (summary.more) 1 else 0,
                            SyncTraceField.PROVIDER_ERROR to if (summary.providerError) 1 else 0,
                            SyncTraceField.CORE_ERROR to if (summary.coreError) 1 else 0,
                        )
                    }
                }
                // Provider and core failures retry with WorkManager's exponential backoff, never a tight append.
                if (summary.providerError || summary.coreError) {
                    if (diagnostic != null) SyncDiagnostics.withTrace(traceId) {
                        SyncDiagnostics.record(SyncTraceEvent.CAPTURE_RETRY)
                    }
                    outcome = WORK_OUTCOME_RETRY
                    return@serialized Result.retry()
                }
                // One bounded follow-up covers every hint that arrived during this pass.
                if (summary.more || SmsCaptureWork.takeHint()) {
                    if (diagnostic != null) {
                        SyncDiagnostics.withTrace(traceId) {
                            SyncDiagnostics.record(SyncTraceEvent.CAPTURE_CONTINUATION)
                            SmsCaptureWork.enqueueContinuation(applicationContext)
                        }
                    } else {
                        SmsCaptureWork.enqueueContinuation(applicationContext)
                    }
                }
                succeeded = true
                outcome = WORK_OUTCOME_SUCCESS
                Result.success()
            } finally {
                if (diagnostic != null) SyncDiagnostics.withTrace(traceId) {
                    SyncDiagnostics.record(SyncTraceEvent.CAPTURE_END, SyncTraceField.SUCCESS to if (succeeded) 1 else 0, SyncTraceField.OUTCOME to outcome)
                }
            }
        }
    }
}

private const val WORK_OUTCOME_SUCCESS = 1L
private const val WORK_OUTCOME_RETRY = 2L
private const val WORK_OUTCOME_FAILURE = 3L
private const val WORK_OUTCOME_THROW = 4L

private data class CaptureWorkerDiagnostic(val traceId: Long, val enqueuedMs: Long?, val source: Long, val unavailable: Boolean) {
    fun delayFields(): Array<Pair<SyncTraceField, Long>> = when {
        unavailable || enqueuedMs == null -> arrayOf(SyncTraceField.UNAVAILABLE to 1L)
        else -> {
            val delay = SyncDiagnostics.nowMs() - enqueuedMs
            if (delay < 0) arrayOf(SyncTraceField.UNAVAILABLE to 1L)
            else arrayOf(SyncTraceField.DELAY_MS to delay)
        }
    }
}

private fun SmsCaptureWorker.captureDiagnosticInput(): CaptureWorkerDiagnostic {
    val data = inputData
    val sameSession = data.keyValueMap.containsKey(SmsCaptureWork.DIAGNOSTIC_SESSION) &&
        data.getLong(SmsCaptureWork.DIAGNOSTIC_SESSION, 0L) == SyncDiagnostics.currentSessionToken()
    val hasTimestamp = data.keyValueMap.containsKey(SmsCaptureWork.DIAGNOSTIC_ENQUEUED_MS)
    val source = if (data.keyValueMap.containsKey(SmsCaptureWork.DIAGNOSTIC_SOURCE))
        data.getLong(SmsCaptureWork.DIAGNOSTIC_SOURCE, SmsCaptureRequestSource.UNKNOWN.code) else SmsCaptureRequestSource.UNKNOWN.code
    return CaptureWorkerDiagnostic(
        traceId = if (sameSession && data.keyValueMap.containsKey(SmsCaptureWork.DIAGNOSTIC_TRACE_ID)) data.getLong(SmsCaptureWork.DIAGNOSTIC_TRACE_ID, 0L).takeIf { it != 0L } ?: SyncDiagnostics.newTraceId() else SyncDiagnostics.newTraceId(),
        enqueuedMs = if (sameSession && hasTimestamp) data.getLong(SmsCaptureWork.DIAGNOSTIC_ENQUEUED_MS, 0L) else null,
        source = source,
        unavailable = !sameSession || !hasTimestamp,
    )
}

/**
 * Bounded provider acquisition. A caller obtains [SmsCapturePlan] from Rust; it must never persist
 * history baselines or completion in Android preferences.
 */
internal object SmsCapture {
    private const val PAGE = 50
    private const val RECHECK_LIMIT = 50
    private const val PASS_BUDGET_NANOS = 10_000_000_000L
    internal const val PROVIDER_SCOPE = "android-sms-provider"

    fun run(
        plan: SmsCapturePlan,
        core: SmsCaptureCore,
        provider: SmsCaptureProvider,
        nanoTime: () -> Long = System::nanoTime,
    ): SmsCaptureSummary = runInternal(plan, core, provider, nanoTime, recheckPending = true)

    /** A multi-plan host invocation reconciles global pending rows exactly once. */
    fun runWithoutPendingRecheck(
        plan: SmsCapturePlan,
        core: SmsCaptureCore,
        provider: SmsCaptureProvider,
        nanoTime: () -> Long = System::nanoTime,
    ): SmsCaptureSummary = runInternal(plan, core, provider, nanoTime, recheckPending = false)

    /** Re-observes a bounded ascending provider page without altering forward/history checkpoints. */
    fun reconcile(
        plan: SmsReconciliationPlan,
        core: SmsCaptureCore,
        provider: SmsCaptureProvider,
        nanoTime: () -> Long = System::nanoTime,
    ): SmsReconciliationResult {
        val deadline = nanoTime() + PASS_BUDGET_NANOS
        fun expired() = nanoTime() >= deadline
        return try {
            val page = provider.page(plan.cursor, PAGE)
            var result = SmsCaptureSummary()
            var cursor = plan.cursor
            for (row in page.records) {
                if (row.id > plan.upperBound) break
                if (expired()) return SmsReconciliationResult(result.copy(more = true), cursor, false)
                val imported = plan.historyUpperBound != null && row.id <= plan.historyUpperBound
                val outcome = process(core, row, plan.sourceGeneration, imported, existing = null)
                result = merge(result, outcome.summary())
                // A paused history row is left for the history scan, which has not passed it yet.
                if (outcome.coreError || outcome.providerError) return SmsReconciliationResult(result, cursor, false)
                cursor = row.id
            }
            val exhausted = page.records.isEmpty() || page.records.any { it.id > plan.upperBound } ||
                page.records.size < PAGE
            SmsReconciliationResult(result.copy(more = result.more || !exhausted), cursor, exhausted)
        } catch (_: SmsProviderException) {
            SmsReconciliationResult(SmsCaptureSummary(more = true, providerError = true), plan.cursor, false)
        } catch (_: Exception) {
            SmsReconciliationResult(SmsCaptureSummary(coreError = true), plan.cursor, false)
        }
    }

    private fun runInternal(
        plan: SmsCapturePlan,
        core: SmsCaptureCore,
        provider: SmsCaptureProvider,
        nanoTime: () -> Long,
        recheckPending: Boolean,
    ): SmsCaptureSummary {
        val deadline = nanoTime() + PASS_BUDGET_NANOS
        fun expired() = nanoTime() >= deadline
        return try {
            val rechecked = if (recheckPending) recheckPending(plan.pendingRecheckCursor, core, provider, ::expired) else SmsCaptureSummary()
            val budgetExpired = expired()
            when {
                // A capacity-paused history plan still lets the pending queue drain above.
                plan.paused -> rechecked.copy(paused = rechecked.paused + 1, more = rechecked.more || budgetExpired)
                budgetExpired || plan.historyComplete || rechecked.coreError || rechecked.providerError -> rechecked.copy(more = rechecked.more || budgetExpired)
                else -> merge(rechecked, scan(plan, core, provider, ::expired))
            }
        } catch (_: SmsProviderException) {
            // A permission/provider error is visible to scheduling/status rather than a false completion.
            SmsCaptureSummary(more = true, providerError = true, pendingRecheckCursor = plan.pendingRecheckCursor)
        } catch (_: Exception) {
            // A core checkpoint/state write failed: retry with backoff, never report a provider outage.
            SmsCaptureSummary(coreError = true, pendingRecheckCursor = plan.pendingRecheckCursor)
        }
    }

    private fun recheckPending(cursor: String?, core: SmsCaptureCore, provider: SmsCaptureProvider, expired: () -> Boolean): SmsCaptureSummary {
        val page = core.acquisitions(cursor, RECHECK_LIMIT)
        var result = SmsCaptureSummary()
        // Rust owns the cursor and rotates its durable ordering, including backlogs larger than one page.
        for (acquisition in page) {
            // The cursor only passes a row once it was actually rechecked, so a failed read or
            // core call retries that same row next time instead of waiting for a full wrap.
            val previous = result.pendingRecheckCursor ?: cursor
            if (expired()) return result.copy(more = true, pendingRecheckCursor = previous)
            if (acquisition.state != SmsAcquisitionState.PENDING) {
                result = result.copy(pendingRecheckCursor = acquisition.id)
                continue
            }
            val providerId = acquisition.input.providerMessageId.toLongOrNull()
            if (providerId == null) {
                result = merge(result, unavailable(core, acquisition, "provider_id_invalid").summary()).copy(pendingRecheckCursor = acquisition.id)
                continue
            }
            val row = try {
                provider.read(providerId)
            } catch (_: SmsProviderException) {
                return result.copy(more = true, providerError = true, pendingRecheckCursor = previous)
            }
            val outcome = if (row == null) unavailable(core, acquisition, "provider_row_missing") else process(core, row, acquisition.input.sourceGeneration, acquisition.input.imported, acquisition)
            result = merge(result, outcome.summary())
            if (outcome.coreError) return result.copy(pendingRecheckCursor = previous)
            result = result.copy(pendingRecheckCursor = acquisition.id)
        }
        return result
    }

    private fun scan(plan: SmsCapturePlan, core: SmsCaptureCore, provider: SmsCaptureProvider, expired: () -> Boolean): SmsCaptureSummary {
        val page = provider.page(plan.checkpoint, PAGE)
        var result = SmsCaptureSummary()
        var checkpointBlocked = false
        for (row in page.records) {
            if (plan.historyUpperBound != null && row.id > plan.historyUpperBound) break
            if (expired()) return result.copy(more = true)
            val outcome = process(core, row, plan.sourceGeneration, plan.imported, null)
            result = merge(result, outcome.summary())
            // Only a durably recorded row (captured, pending or unavailable) may pass the checkpoint.
            // Backpressure or a core failure stops this pass at the row; it is retried, not skipped.
            if (!outcome.providerDurable) {
                checkpointBlocked = true
                break
            }
            core.setCheckpoint(plan.sourceGeneration, plan.subscriptionId, plan.imported, row.id.toString())
        }
        val reachedUpperBound = plan.historyUpperBound != null && page.records.any { it.id > plan.historyUpperBound }
        val historyExhausted = plan.imported && plan.historyUpperBound != null && !checkpointBlocked &&
            (reachedUpperBound || page.records.isEmpty() || page.scannedThroughId >= plan.historyUpperBound)
        return result.copy(
            more = result.more || (!checkpointBlocked && page.records.size == PAGE && !reachedUpperBound),
            historyExhausted = historyExhausted,
        )
    }

    private fun process(core: SmsCaptureCore, row: SmsProviderRecord, sourceGeneration: String, imported: Boolean, existing: SmsAcquisition?): Outcome {
        if (existing != null) {
            val refreshedSource = input(row, existing.input.sourceGeneration, existing.input.imported)
            if (row.id.toString() != existing.input.providerMessageId ||
                refreshedSource.subscriptionId != existing.input.subscriptionId ||
                refreshedSource.providerThreadId != existing.input.providerThreadId
            ) {
                return unavailable(core, existing, "provider_source_changed")
            }
        }
        val acquisition = try {
            // Re-beginning refreshes mutable fields of the same immutable evidence; changed
            // evidence (for example a failed send retried with a new date) supersedes the old row.
            core.begin(input(row, existing?.input?.sourceGeneration ?: sourceGeneration, existing?.input?.imported ?: imported))
        } catch (_: SmsCaptureCapacityException) {
            return Outcome(providerDurable = false, paused = 1)
        } catch (_: Exception) {
            return Outcome(providerDurable = false, coreError = true)
        }
        when (acquisition.state) {
            SmsAcquisitionState.COMPLETE -> return Outcome(providerDurable = true)
            SmsAcquisitionState.PAUSED -> return Outcome(providerDurable = true, paused = 1)
            // Core-owned terminal state (unrepresentable row or ambiguous reconciliation).
            SmsAcquisitionState.UNAVAILABLE -> return Outcome(providerDurable = true, unavailable = 1)
            SmsAcquisitionState.PENDING -> {}
        }
        when (row.type) {
            OUTBOX, QUEUED -> return pending(core, acquisition, "provider_box_pending")
            // Not messages yet, but they may still be sent: rechecked without counting as backlog.
            DRAFT -> return pending(core, acquisition, REASON_DRAFT)
            FAILED -> return pending(core, acquisition, REASON_FAILED)
            INBOX, SENT -> {}
            else -> return unavailable(core, acquisition, "provider_type_unavailable")
        }
        if (row.body == null) return unavailable(core, acquisition, "provider_body_missing")
        return try {
            when (core.complete(acquisition.id)) {
                SmsCompletion.CAPTURED -> Outcome(providerDurable = true, captured = 1)
                SmsCompletion.DUPLICATE -> Outcome(providerDurable = true)
                SmsCompletion.UNAVAILABLE -> Outcome(providerDurable = true, unavailable = 1)
            }
        } catch (_: Exception) {
            // Durable and still pending: the recheck retries it after WorkManager backoff.
            Outcome(providerDurable = true, pending = 1, coreError = true)
        }
    }

    private fun pending(core: SmsCaptureCore, acquisition: SmsAcquisition, reason: String): Outcome = try {
        core.setState(acquisition.id, SmsAcquisitionState.PENDING, reason)
        Outcome(providerDurable = true, pending = 1)
    } catch (_: Exception) {
        Outcome(providerDurable = true, pending = 1, coreError = true)
    }

    private fun input(row: SmsProviderRecord, sourceGeneration: String, imported: Boolean): SmsAcquisitionInput {
        // A missing subscription is source evidence, not a routing decision. Keeping it in this
        // non-routable namespace lets the durable core advance the provider checkpoint.
        val subscription = row.subscriptionId?.let(SimRoutes::routeId) ?: UNKNOWN_SUBSCRIPTION
        val incoming = row.type == INBOX
        return SmsAcquisitionInput(
            sourceGeneration, subscription, row.id.toString(), row.threadId?.toString(),
            incoming, row.address, if (incoming) emptyList() else listOfNotNull(row.address), row.body.orEmpty(), imported,
            row.dateMs, row.dateSentMs, row.creator, sourceRowFingerprint(row),
            broadcastId = if (incoming) broadcastId(row) else null,
        )
    }

    /** The identity [IncomingCapture] gave the same message: sender, SMSC time and full text. */
    private fun broadcastId(row: SmsProviderRecord): String? {
        val address = row.address ?: return null
        val smsc = row.dateSentMs ?: return null
        val body = row.body ?: return null
        return IncomingCapture.broadcastId(address, smsc, body)
    }

    /** Immutable provider evidence: excludes box/status/body/date_sent fields that may settle in place. */
    internal fun sourceRowFingerprint(row: SmsProviderRecord): String {
        val digest = java.security.MessageDigest.getInstance("SHA-256")
        fun field(value: String?) {
            val bytes = (value ?: "<null>").toByteArray(Charsets.UTF_8)
            digest.update(java.nio.ByteBuffer.allocate(Int.SIZE_BYTES).putInt(bytes.size).array())
            digest.update(bytes)
        }
        field("android-sms-row-v1")
        field(row.id.toString())
        field(row.threadId?.toString())
        field(row.dateMs?.toString())
        field(row.address?.trim()?.lowercase()?.filterNot(Char::isWhitespace))
        field(row.creator)
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun unavailable(core: SmsCaptureCore, acquisition: SmsAcquisition, reason: String): Outcome = try {
        core.setState(acquisition.id, SmsAcquisitionState.UNAVAILABLE, reason)
        Outcome(providerDurable = true, unavailable = 1)
    } catch (_: Exception) {
        Outcome(providerDurable = true, coreError = true)
    }

    private fun merge(left: SmsCaptureSummary, right: SmsCaptureSummary) = SmsCaptureSummary(
        left.captured + right.captured, left.pending + right.pending, left.unavailable + right.unavailable,
        left.paused + right.paused, left.more || right.more, left.providerError || right.providerError,
        right.pendingRecheckCursor ?: left.pendingRecheckCursor, left.historyExhausted || right.historyExhausted,
        left.coreError || right.coreError,
    )

    private data class Outcome(val providerDurable: Boolean, val captured: Int = 0, val pending: Int = 0, val unavailable: Int = 0, val paused: Int = 0, val providerError: Boolean = false, val coreError: Boolean = false) {
        fun summary() = SmsCaptureSummary(captured, pending, unavailable, paused, providerError = providerError, coreError = coreError)
    }
    private const val INBOX = 1
    private const val SENT = 2
    private const val DRAFT = 3
    private const val OUTBOX = 4
    private const val FAILED = 5
    private const val QUEUED = 6
    internal const val UNKNOWN_SUBSCRIPTION = "android-sms-unknown-subscription"
    /** Core reason codes (`SMS_REASON_DRAFT`/`SMS_REASON_FAILED`): rechecked, not reported as backlog. */
    internal const val REASON_DRAFT = "provider_box_draft"
    internal const val REASON_FAILED = "provider_box_failed"
}
