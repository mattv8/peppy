package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.BatteryManager
import androidx.core.content.ContextCompat
import androidx.work.BackoffPolicy
import androidx.work.CoroutineWorker
import androidx.work.Data
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.ListenableWorker
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import dev.peppy.mobile.diagnostics.MmsCaptureScanDiagnostics
import dev.peppy.mobile.diagnostics.MmsDiagnosticStage
import dev.peppy.mobile.diagnostics.MmsPassDiagnostics
import dev.peppy.mobile.diagnostics.SyncDiagnosticStage
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.captureMmsPass
import dev.peppy.mobile.diagnostics.captureMmsProgress
import dev.peppy.mobile.diagnostics.captureMmsStage
import dev.peppy.mobile.diagnostics.captureTimed
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.MessageHistoryRequestCoordinator
import dev.peppy.mobile.gateway.MmsHistoryCaptureAdmission
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.mms.MmsBackfillCore
import dev.peppy.mobile.mms.MmsBackfillPageResult
import dev.peppy.mobile.mms.MmsBackfillPhase
import dev.peppy.mobile.mms.MmsBackfillRecheck
import dev.peppy.mobile.mms.MmsBackfillRecheckConditions
import dev.peppy.mobile.mms.MmsBackfillRecheckOutcome
import dev.peppy.mobile.mms.MmsBackfillRecheckRequest
import dev.peppy.mobile.mms.MmsBackfillRecheckState
import dev.peppy.mobile.mms.MmsBackfillScanPlan
import dev.peppy.mobile.mms.MmsBackfillScanner
import dev.peppy.mobile.mms.MmsCaptureHintScheduler
import dev.peppy.mobile.mms.MmsCaptureHints
import dev.peppy.mobile.mms.MmsCaptureScheduleSink
import dev.peppy.mobile.mms.MmsCaptureSummary
import dev.peppy.mobile.mms.MmsLegacyBackfillState
import dev.peppy.mobile.mms.MmsMediaPreview
import dev.peppy.mobile.mms.MmsMediaPreviewResult
import dev.peppy.mobile.mms.MmsMediaPreviewUnavailable
import dev.peppy.mobile.mms.MmsPreferences
import dev.peppy.mobile.mms.MmsProviderException
import dev.peppy.mobile.mms.MmsProviderPage
import dev.peppy.mobile.mms.MmsProviderPart
import dev.peppy.mobile.mms.MmsProviderReader
import dev.peppy.mobile.mms.MmsProviderRecord
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.mms.mmsReceiveGranted
import dev.peppy.mobile.mms.restrictedMessageAccess
import dev.peppy.mobile.sms.MmsCaptureLifecycle
import dev.peppy.mobile.telephony.OwnNumberDetector
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.security.MessageDigest
import java.util.Properties
import java.util.concurrent.TimeUnit
import kotlin.math.min
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMmsAcquisition
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeMmsBackfillDirection
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPhase
import uniffi.peppy_mobile_bindings.NativeMmsBackfillRecheckConditions
import uniffi.peppy_mobile_bindings.NativeMmsBackfillRecheckOutcome
import uniffi.peppy_mobile_bindings.NativeMmsBeginOutcome
import uniffi.peppy_mobile_bindings.NativeMmsLegacyBackfillState
import uniffi.peppy_mobile_bindings.NativeMmsPreviewState
import uniffi.peppy_mobile_bindings.NativeMmsSource


/** Binding from scheduled work to one bounded capture pass; replaced only by scheduling tests. */
internal fun interface MmsCaptureRunner {
    fun run(context: Context, manualRecheck: Boolean, providerChanged: Boolean): MmsCaptureSummary
}

/** Offline-only reconciliation for provider rows. Network upload remains GatewayWork's concern. */
object MmsCaptureWork {
    const val NAME = "peppy-mms-capture"
    const val PERIODIC_NAME = "peppy-mms-capture-periodic"

    @Volatile internal var runner = MmsCaptureRunner { context, manualRecheck, providerChanged ->
        MmsCapture.run(context, manualRecheck, providerChanged)
    }

    fun enqueue(context: Context) = hintScheduler.requestNormal(scheduleSink(context))
    fun refresh(context: Context) = hintScheduler.replace(scheduleSink(context, manualRecheck = true))

    /** Provider observer and WAP hints bypass unchanged-failure backoff for one coalesced pass. */
    fun enqueueProviderHint(context: Context) {
        MmsCaptureHints.record(context)
        hintScheduler.requestHint(scheduleSink(context))
    }

    /** Starts provider observation and bounded reconciliation after a permission or foreground change. */
    fun onPermissionOrForeground(context: Context) {
        MmsCaptureLifecycle.ensureObserver(context)
        if (MmsPreferences(context).enabled && mmsReceiveGranted(context)) {
            enqueue(context.applicationContext)
            ensurePeriodic(context.applicationContext)
        }
    }

    internal data class DeferredCapture(val manualRecheck: Boolean, val providerChanged: Boolean)

    internal fun deferUntilProducerReady(context: Context, manualRecheck: Boolean, providerChanged: Boolean) {
        val prefs = context.getSharedPreferences(DEFERRED_CAPTURE, Context.MODE_PRIVATE)
        prefs.edit()
            .putBoolean(DEFERRED_MANUAL, prefs.getBoolean(DEFERRED_MANUAL, false) || manualRecheck)
            .putBoolean(DEFERRED_PROVIDER, prefs.getBoolean(DEFERRED_PROVIDER, false) || providerChanged)
            .commit()
    }

    internal fun consumeDeferredCapture(context: Context): DeferredCapture? {
        val prefs = context.getSharedPreferences(DEFERRED_CAPTURE, Context.MODE_PRIVATE)
        val manual = prefs.getBoolean(DEFERRED_MANUAL, false)
        val provider = prefs.getBoolean(DEFERRED_PROVIDER, false)
        if (!manual && !provider && !prefs.contains(DEFERRED_MANUAL)) return null
        if (!prefs.edit().clear().commit()) return null
        return DeferredCapture(manual, provider)
    }

    internal fun resumeDeferred(context: Context) {
        val deferred = consumeDeferredCapture(context) ?: return
        if (deferred.providerChanged) MmsCaptureHints.record(context)
        if (deferred.manualRecheck) refresh(context) else enqueue(context)
    }

    internal fun handleResult(context: Context, result: MmsCaptureSummary) {
        if (result.captured > 0) GatewayWork.enqueueAfterCapture(context)
        hintScheduler.requestContinuation(result.more, scheduleSink(context))
    }

    fun ensurePeriodic(context: Context) {
        WorkManager.getInstance(context).enqueueUniquePeriodicWork(
            PERIODIC_NAME,
            ExistingPeriodicWorkPolicy.KEEP,
            PeriodicWorkRequestBuilder<MmsCaptureWorker>(15, TimeUnit.MINUTES)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build(),
        )
    }

    private fun schedule(
        context: Context,
        name: String,
        policy: ExistingWorkPolicy,
        delayed: Boolean,
        manualRecheck: Boolean,
        managedWork: Boolean,
    ) {
        val request = OneTimeWorkRequestBuilder<MmsCaptureWorker>()
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
            .setInputData(workDataOf(MANUAL_RECHECK to manualRecheck, MANAGED_WORK to managedWork))
            .apply { if (delayed) setInitialDelay(15, TimeUnit.SECONDS) }
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(name, policy, request)
    }

    private fun scheduleSink(context: Context, manualRecheck: Boolean = false) = MmsCaptureScheduleSink { policy, delayed ->
        schedule(context, NAME, policy, delayed, manualRecheck, managedWork = true)
    }

    private const val DEFERRED_CAPTURE = "peppy-mms-retention-deferred"
    private const val DEFERRED_MANUAL = "manual"
    private const val DEFERRED_PROVIDER = "provider"
    internal const val MANUAL_RECHECK = "manual-mms-recheck"
    internal const val MANAGED_WORK = "managed-mms-capture-work"
    internal val hintScheduler = MmsCaptureHintScheduler()

    internal fun isManagedData(inputData: Data): Boolean =
        if (inputData.keyValueMap.containsKey(MANAGED_WORK)) inputData.getBoolean(MANAGED_WORK, false)
        else inputData.keyValueMap.containsKey(MANUAL_RECHECK)

    internal fun runWorkerPass(
        context: Context,
        inputData: Data,
        onSuccess: (MmsCaptureSummary) -> Unit = {},
        run: (manualRecheck: Boolean, providerChanged: Boolean) -> MmsCaptureSummary,
    ): ListenableWorker.Result {
        val managedWork = isManagedData(inputData)
        hintScheduler.beginPass(managedWork)
        var consumedHint = false
        var completed = false
        try {
            consumedHint = MmsCaptureHints.consumeProviderChange(context)
            val result = run(inputData.getBoolean(MANUAL_RECHECK, false), consumedHint)
            handleResult(context, result)
            onSuccess(result)
            completed = true
            return ListenableWorker.Result.success()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            return ListenableWorker.Result.retry()
        } finally {
            try {
                if (!completed && consumedHint) MmsCaptureHints.record(context)
            } finally {
                hintScheduler.finishPass(managedWork)
            }
        }
    }
}

class MmsCaptureWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result = PASS.withLock {
        withContext(Dispatchers.IO) {
            val job = currentCoroutineContext()[kotlinx.coroutines.Job]
            var more = false
            val result = MmsCaptureWork.runWorkerPass(
                applicationContext,
                inputData,
                onSuccess = { more = it.more },
                run = { manualRecheck, providerChanged ->
                    MmsCaptureWork.runner.run(applicationContext, manualRecheck, providerChanged).also {
                        job?.ensureActive()
                    }
                },
            )
            if (result == ListenableWorker.Result.success() && PhoneReadCaptureWork.shouldRun(more)) {
                PhoneReadCaptureWork.run(applicationContext)
            }
            result
        }
    }

    private companion object { val PASS = Mutex() }
}

internal data class MmsCapturePlan(
    val generation: String,
    val route: SimRoute?,
    val importHistory: Boolean,
    val historyBaseline: Long?,
    val recheckCursor: String?,
    val manualRecheck: Boolean,
    val charging: Boolean = false,
    val providerChanged: Boolean = false,
)

internal interface MmsCaptureProvider {
    fun highWater(): Long
    fun page(afterId: Long, limit: Int): MmsProviderPage
    fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int): MmsProviderPage
    fun read(id: Long): MmsProviderRecord?
    fun readPartText(part: MmsProviderPart, byteLimit: Int): String?
    fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long): Long
    fun inspectMedia(file: File, contentType: String?): MmsMediaPreviewResult = MmsMediaPreview().inspect(file, contentType)
}

internal interface MmsCaptureCore : MmsBackfillCore {
    fun begin(input: NativeMmsAcquisitionInput): NativeMmsBeginOutcome
    fun requestManualRecheck(generation: String, scope: String, limit: Int): Int
    fun setState(id: String, state: NativeMmsAcquisitionState, reason: String?)
    fun mappedParts(id: String): Map<String, String>
    fun previewStates(id: String): Map<String, NativeMmsPreviewState>
    fun pendingMediaBytes(): Long
    fun prepareAttachment(path: String, mediaType: String, name: String): String
    fun mapPart(id: String, providerPartId: String, attachmentId: String)
    fun setPartMetadata(
        id: String,
        providerPartId: String,
        width: Int?,
        height: Int?,
        durationMs: Long?,
        thumbnailAttachmentId: String?,
    )
    fun setPartPreviewState(id: String, providerPartId: String, state: NativeMmsPreviewState)
    fun discardAttachment(id: String)
    fun complete(id: String)
}

/** Provider orchestration is injected so tests exercise checkpoint/core boundary ordering. */
internal object MmsCapture {
    private const val PAGE = 50
    internal const val PROVIDER_SCOPE = "android-mms-provider"
    private const val MAX_PENDING_RECHECKS = 32
    private const val PASS_BUDGET_NANOS = 10_000_000_000L
    private const val MAX_BYTES = 32L * 1024 * 1024
    private const val MAX_BINARY = 10
    private const val PENDING_MEDIA_LIMIT = 512L * 1024 * 1024
    private val POST_SETTLE_REASONS = setOf(
        "preview_deferred", "media_quota", "parts_incomplete", "text_unavailable",
        "preview_unavailable", "part_unavailable", "preview_metadata_unavailable",
    )

    fun run(context: Context, manualRecheck: Boolean = false, providerChanged: Boolean = false): MmsCaptureSummary {
        val prefs = MmsPreferences(context)
        val eligible = syncOwnNumberForMmsCapture(
            mmsEnabled = prefs.enabled,
            permissionGranted = { granted(context) },
            policyAllows = { GatewayPolicyHost(context).decision()?.captureMms == true },
        ) {
            OwnNumberDetector.syncCurrentRoute(context)
        }
        if (!eligible) return MmsCaptureSummary(0, 0, 0, false)
        val native = NativeGateway.producerClient(context) ?: run {
            MmsCaptureWork.deferUntilProducerReady(context, manualRecheck, providerChanged)
            GatewayWork.enqueue(context.applicationContext)
            return MmsCaptureSummary(0, 0, 0, false)
        }
        val route = SimRoutes.current().singleOrNull()
        val reader = MmsProviderReader(context)
        val provider = object : MmsCaptureProvider {
            override fun highWater() = currentHighWater(context)
            override fun page(afterId: Long, limit: Int) = reader.page(afterId, limit)
            override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
                reader.descendingPage(cursor, upperBound, floor, limit)
            override fun read(id: Long) = reader.read(id)
            override fun readPartText(part: MmsProviderPart, byteLimit: Int) = reader.readPartText(part, byteLimit)
            override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long) = reader.copyPart(part, destination, byteLimit)
        }
        val plan = MmsCapturePlan(
            prefs.sourceGeneration,
            route,
            prefs.importHistory,
            prefs.historyBaseline(),
            prefs.recheckCursor(),
            manualRecheck,
            charging = context.getSystemService(BatteryManager::class.java)?.isCharging == true,
            providerChanged = providerChanged,
        )
        return run(
            plan,
            NativeCore(native),
            provider,
            context.noBackupFilesDir,
            onControlPublished = { GatewayWork.enqueueAfterCapture(context.applicationContext) },
            restrictedAccess = { restrictedMessageAccess(context) },
        )
    }

    internal fun syncOwnNumberForMmsCapture(
        mmsEnabled: Boolean,
        permissionGranted: () -> Boolean,
        policyAllows: () -> Boolean,
        sync: () -> Unit,
    ): Boolean {
        if (!mmsEnabled || !permissionGranted() || !policyAllows()) return false
        try {
            sync()
        } catch (_: Exception) {
            // Carrier metadata detection is best-effort and must not stop MMS capture.
        }
        return true
    }

    internal fun run(
        plan: MmsCapturePlan,
        core: MmsCaptureCore,
        provider: MmsCaptureProvider,
        tempDirectory: File,
        onControlPublished: () -> Unit = {},
        restrictedAccess: () -> RestrictedMessageAccess = { RestrictedMessageAccess.ALLOWED },
    ): MmsCaptureSummary {
        var captured = 0
        var pending = 0
        var unavailable = 0
        var followup = false
        val diagnostics = MmsPassDiagnostics()
        fun recordProviderFailure(kind: ProviderReadKind, error: Exception) {
            diagnostics.providerFailures++
            when (kind) {
                ProviderReadKind.HIGH_WATER_OR_PAGE -> diagnostics.providerFailureHighWaterPage++
                ProviderReadKind.ROW_READ -> diagnostics.providerFailureRowRead++
                ProviderReadKind.PART_TEXT_READ -> diagnostics.providerFailurePartTextRead++
                ProviderReadKind.PART_COPY -> diagnostics.providerFailurePartCopy++
            }
            when (error) {
                is SecurityException -> diagnostics.providerFailureSecurity++
                is IOException -> diagnostics.providerFailureIo++
                is MmsProviderException -> diagnostics.providerFailureProvider++
                is IllegalStateException, is IllegalArgumentException -> diagnostics.providerFailureStateArgument++
                else -> diagnostics.providerFailureOther++
            }
        }
        fun <T> providerRead(kind: ProviderReadKind, pageQuery: Boolean = false, block: () -> T): T {
            diagnostics.providerReads++
            if (pageQuery) diagnostics.pageQueries++
            return SyncDiagnostics.measureProvider(
                onMeasured = { durationMs, cpuMs ->
                    diagnostics.providerDurationMs += durationMs
                    diagnostics.providerCpuMs += cpuMs
                },
            ) {
                try {
                    block()
                } catch (error: Exception) {
                    recordProviderFailure(kind, error)
                    throw error
                }
            }
        }
        val diagnosticProvider = object : MmsCaptureProvider by provider {
            override fun highWater() = providerRead(ProviderReadKind.HIGH_WATER_OR_PAGE) { provider.highWater() }
            override fun page(afterId: Long, limit: Int) = providerRead(ProviderReadKind.HIGH_WATER_OR_PAGE, pageQuery = true) { provider.page(afterId, limit) }
            override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
                providerRead(ProviderReadKind.HIGH_WATER_OR_PAGE, pageQuery = true) { provider.descendingPage(cursor, upperBound, floor, limit) }
            override fun read(id: Long) = providerRead(ProviderReadKind.ROW_READ) { provider.read(id) }
            override fun readPartText(part: MmsProviderPart, byteLimit: Int) = providerRead(ProviderReadKind.PART_TEXT_READ) { provider.readPartText(part, byteLimit) }
            override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long) =
                providerRead(ProviderReadKind.PART_COPY) { provider.copyPart(part, destination, byteLimit) }
        }
        val deadline = System.nanoTime() + PASS_BUDGET_NANOS
        return captureMmsPass(diagnostics, { followup }) {
            val highWater = captureMmsStage(MmsDiagnosticStage.HIGH_WATER) { diagnosticProvider.highWater() }
            if (plan.manualRecheck) {
                try {
                    core.requestManualRecheck(plan.generation, PROVIDER_SCOPE, MAX_PENDING_RECHECKS)
                } catch (_: Exception) {
                    // A reset is best-effort; the existing bounded plan must still run.
                }
            }
            fun budgetExpired() = System.nanoTime() >= deadline
            val scanner = MmsBackfillScanner(core)
            val history = (core as? NativeCore)?.let {
                MessageHistoryRequestCoordinator(it.client, onControlPublished)
            }
            followup = captureTimed(SyncDiagnosticStage.MMS) {
                scanner.run(
                    plan.generation,
                    PROVIDER_SCOPE,
                    MmsLegacyBackfillState(
                        plan.historyBaseline?.toString(),
                        plan.importHistory,
                        plan.recheckCursor,
                    ),
                    highWater.toString(),
                    System.currentTimeMillis(),
                    MmsBackfillRecheckConditions(
                        charging = plan.charging,
                        capacityChanged = false,
                        explicitRefresh = plan.manualRecheck,
                        providerChanged = plan.providerChanged,
                    ),
                    scanPage = scanPage@{ backfillPlan ->
                        if (backfillPlan.phase == MmsBackfillPhase.PROVIDER_RESCAN &&
                            restrictedAccess() != RestrictedMessageAccess.ALLOWED
                        ) {
                            return@scanPage MmsBackfillPageResult(null, exhausted = false, more = false, runnable = false)
                        }
                        val provider = if (backfillPlan.phase == MmsBackfillPhase.PROVIDER_RESCAN) {
                            accessFencedProvider(diagnosticProvider, restrictedAccess)
                        } else {
                            diagnosticProvider
                        }
                        val importsHistory = backfillPlan.phase == MmsBackfillPhase.HISTORY ||
                            backfillPlan.phase == MmsBackfillPhase.PROVIDER_RESCAN
                        val result = try {
                            scan(
                                core, provider, tempDirectory, plan.generation,
                                imported = importsHistory,
                                cursor = backfillPlan.cursor?.toLongOrNull(),
                                upperBound = if (importsHistory) backfillPlan.historyUpperBound?.toLongOrNull() else null,
                                descending = importsHistory && backfillPlan.descending,
                                floor = backfillPlan.historyFloor?.toLongOrNull(),
                                budget = backfillPlan.budget.coerceIn(1, PAGE),
                                budgetExpired = ::budgetExpired,
                            )
                        } catch (_: RestrictedMessageAccessRevokedException) {
                            return@scanPage MmsBackfillPageResult(null, exhausted = false, more = false, runnable = false)
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            return@scanPage MmsBackfillPageResult(null, exhausted = false, more = false, runnable = false)
                        }
                        diagnostics.add(result.diagnostics())
                        captured += result.captured
                        pending += result.pending
                        unavailable += result.unavailable
                        followup = followup || result.more
                        captureMmsProgress(
                            rowsQueried = result.rowsQueried,
                            processed = result.processed,
                            advanced = result.lastDurableId != null,
                            budgetExhausted = result.budgetExhausted,
                            captured = result.captured,
                            pending = result.pending,
                            unavailable = result.unavailable,
                            more = result.more,
                        )
                        val stillAllowed = backfillPlan.phase != MmsBackfillPhase.PROVIDER_RESCAN ||
                            restrictedAccess() == RestrictedMessageAccess.ALLOWED
                        MmsBackfillPageResult(
                            result.lastDurableId,
                            result.exhausted,
                            result.more,
                            observations = result.rowsQueried,
                            runnable = stillAllowed,
                        )
                    },
                    recheck = recheck@{ request ->
                    fun recordOutcome(value: MmsBackfillRecheckOutcome): MmsBackfillRecheckOutcome {
                        diagnostics.recordRecheckOutcome(value)
                        return value
                    }
                    if (budgetExpired()) return@recheck null
                    val providerId = request.providerMessageId.toLongOrNull() ?: return@recheck recordOutcome(MmsBackfillRecheckOutcome.TRANSIENT_FAILURE)
                    val record = try { diagnosticProvider.read(providerId) } catch (error: CancellationException) {
                        throw error
                    } catch (_: Exception) {
                        return@recheck recordOutcome(MmsBackfillRecheckOutcome.TRANSIENT_FAILURE)
                    } ?: return@recheck recordOutcome(MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE)
                    val routeId = record.subscriptionId?.takeIf { it >= 0 }?.let(SimRoutes::routeId)
                        ?: return@recheck recordOutcome(MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE)
                    val incoming = record.messageType == RETRIEVE_CONF || record.messageType == NOTIFICATION_IND || record.messageBox == INBOX
                    if (incoming != request.incoming) return@recheck recordOutcome(MmsBackfillRecheckOutcome.PROVIDER_CHANGED)
                    val outcome = try {
                        process(core, diagnosticProvider, tempDirectory, plan.generation, routeId, record, imported = false, existing = null)
                    } catch (error: CancellationException) {
                        throw error
                    } catch (_: Exception) { ProcessResult(durable = true) }
                    captured += outcome.captured
                    pending += outcome.pending
                    unavailable += outcome.unavailable
                    diagnostics.processed++
                    diagnostics.captured += outcome.captured
                    diagnostics.pending += outcome.pending
                    diagnostics.unavailable += outcome.unavailable
                    recordOutcome(when {
                        outcome.verified -> MmsBackfillRecheckOutcome.VERIFIED
                        outcome.captured > 0 || outcome.followup -> MmsBackfillRecheckOutcome.PROGRESS
                        // Carrier content still arriving: the core keeps the fair order and bounded resting schedule.
                        !record.ready && record.error == "provider_content_pending" -> MmsBackfillRecheckOutcome.AWAITING_PROVIDER
                        outcome.partUnavailable -> MmsBackfillRecheckOutcome.UNCHANGED_PART_UNAVAILABLE
                        outcome.unavailable > 0 -> MmsBackfillRecheckOutcome.UNCHANGED_UNAVAILABLE
                        else -> MmsBackfillRecheckOutcome.TRANSIENT_FAILURE
                    })
                    },
                    historyAdmission = history?.let { coordinator -> object : MmsHistoryCaptureAdmission {
                        override fun begin() = coordinator.begin(plan.generation, "mms")
                        override fun finish(admission: MessageHistoryRequestCoordinator.Admission, observations: Int, exhausted: Boolean, reason: String?) =
                            coordinator.finish(plan.generation, admission, observations, exhausted, reason)
                        } },
                    canRunPlan = { scanPlan ->
                        scanPlan.phase != MmsBackfillPhase.PROVIDER_RESCAN || restrictedAccess() == RestrictedMessageAccess.ALLOWED
                    },
                    canCommitProviderRescan = {
                        restrictedAccess() == RestrictedMessageAccess.ALLOWED
                    },
                )
            }
            MmsCaptureSummary(captured, pending, unavailable, followup)
        }
    }

    private fun scan(
        core: MmsCaptureCore,
        provider: MmsCaptureProvider,
        tempDirectory: File,
        generation: String,
        imported: Boolean,
        cursor: Long?,
        upperBound: Long?,
        descending: Boolean,
        floor: Long?,
        budget: Int,
        budgetExpired: () -> Boolean,
    ): ScanResult {
        val page = captureMmsStage(MmsDiagnosticStage.SCAN_PAGE) {
            if (descending) provider.descendingPage(cursor, requireNotNull(upperBound), floor, budget)
            else provider.page(cursor ?: 0L, budget)
        }
        var captured = 0
        var pending = 0
        var unavailable = 0
        var followup = false
        var lastDurableId: String? = null
        var stopped = false
        var checkpointBlocked = false
        var processed = 0
        var budgetExhausted = false
        for (record in page.records) {
            if (!descending && upperBound != null && record.id > upperBound) { stopped = true; break }
            if (budgetExpired()) { followup = true; stopped = true; budgetExhausted = true; break }
            processed++
            val subscriptionId = record.subscriptionId?.takeIf { it >= 0 }
            if (subscriptionId == null) {
                val outcome = try {
                    unavailableUnknownSubscription(core, generation, record, imported)
                } catch (error: RestrictedMessageAccessRevokedException) {
                    throw error
                } catch (_: Exception) {
                    ProcessResult(durable = false)
                }
                captured += outcome.captured
                pending += outcome.pending
                unavailable += outcome.unavailable
                followup = followup || outcome.followup
                if (!outcome.durable) {
                    checkpointBlocked = true
                    if (descending) break
                    continue
                }
                if (!checkpointBlocked) lastDurableId = record.id.toString()
                continue
            }
            val routeId = SimRoutes.routeId(subscriptionId)
            val outcome = try {
                process(core, provider, tempDirectory, generation, routeId, record, imported, null)
            } catch (error: CancellationException) {
                throw error
            } catch (error: RestrictedMessageAccessRevokedException) {
                throw error
            } catch (_: Exception) {
                ProcessResult(durable = false)
            }
            captured += outcome.captured
            pending += outcome.pending
            unavailable += outcome.unavailable
            followup = followup || outcome.followup
            if (!outcome.durable) {
                checkpointBlocked = true
                if (descending) break else continue
            }
            if (!checkpointBlocked) lastDurableId = record.id.toString()
        }
        val reachedBound = !descending && upperBound != null && page.records.any { it.id > upperBound }
        val exhausted = !followup && !(descending && checkpointBlocked) && (page.records.size < budget || reachedBound)
        return ScanResult(
            captured, pending, unavailable, followup || (!checkpointBlocked && !exhausted), lastDurableId,
            exhausted, page.records.size, processed, budgetExhausted,
        )
    }

    private fun process(
        core: MmsCaptureCore,
        provider: MmsCaptureProvider,
        tempDirectory: File,
        generation: String,
        routeId: String,
        record: MmsProviderRecord,
        imported: Boolean,
        existing: NativeMmsAcquisition?,
    ): ProcessResult {
        if (record.messageType !in setOf(SEND_REQ, NOTIFICATION_IND, RETRIEVE_CONF)) {
            existing?.let { core.setState(it.acquisitionId, NativeMmsAcquisitionState.UNAVAILABLE, "provider_non_message_type") }
            return ProcessResult(true, unavailable = if (existing == null) 0 else 1)
        }
        val placeholder = input(generation, routeId, record, imported, body = inlineText(record))
        val acquisition = existing ?: when (val outcome = try { core.begin(placeholder) } catch (_: Exception) { return ProcessResult(false) }) {
            is NativeMmsBeginOutcome.Acquired -> outcome.acquisition
            is NativeMmsBeginOutcome.Quarantined -> return ProcessResult(true)
        }
        when (acquisition.state) {
            NativeMmsAcquisitionState.COMPLETE -> return ProcessResult(true, verified = true)
            NativeMmsAcquisitionState.UNAVAILABLE -> return ProcessResult(true, unavailable = 1)
            NativeMmsAcquisitionState.BLOCKED -> return ProcessResult(true, pending = 1)
            NativeMmsAcquisitionState.PENDING -> Unit
        }
        if (record.messageBox !in setOf(INBOX, SENT)) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "provider_box_pending")
            return ProcessResult(true, pending = 1)
        }
        if (!placeholder.incoming && placeholder.recipients.isEmpty() && record.bccRecipients.isNotEmpty()) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.BLOCKED, "bcc_only_recipients_not_modeled")
            return ProcessResult(true, pending = 1)
        }
        if (!record.ready) {
            val reason = record.error ?: "unsupported_metadata"
            val retryable = reason == "provider_content_pending"
            core.setState(acquisition.acquisitionId, if (retryable) NativeMmsAcquisitionState.PENDING else NativeMmsAcquisitionState.UNAVAILABLE, reason)
            return if (retryable) ProcessResult(true, pending = 1) else ProcessResult(true, unavailable = 1)
        }

        val fingerprint = fingerprint(record)
        val postSettle = acquisition.reason in POST_SETTLE_REASONS
        if (!postSettle && acquisition.reason != "settling:$fingerprint") {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "settling:$fingerprint")
            return ProcessResult(true, pending = 1, followup = true)
        }
        val stable = provider.read(record.id)
        if (stable == null) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.UNAVAILABLE, "provider_row_missing")
            return ProcessResult(true, unavailable = 1)
        }
        val stableFingerprint = fingerprint(stable)
        if (!stable.ready || stable.messageBox !in setOf(INBOX, SENT) || stableFingerprint != fingerprint || (postSettle && !mappedPartsRetained(core, acquisition.acquisitionId, stable))) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "settling:$stableFingerprint")
            return ProcessResult(true, pending = 1, followup = true)
        }
        val body = try { readText(provider, stable) } catch (error: Exception) {
            if (error is RestrictedMessageAccessRevokedException) throw error
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "text_unavailable")
            return ProcessResult(true, pending = 1)
        }
        val updated = when (val outcome = try { core.begin(input(generation, routeId, stable, imported, body)) } catch (_: Exception) { return ProcessResult(false) }) {
            is NativeMmsBeginOutcome.Acquired -> outcome.acquisition
            is NativeMmsBeginOutcome.Quarantined -> return ProcessResult(true)
        }
        return when (updated.state) {
            NativeMmsAcquisitionState.COMPLETE -> ProcessResult(true, verified = true)
            NativeMmsAcquisitionState.UNAVAILABLE -> ProcessResult(true, unavailable = 1)
            NativeMmsAcquisitionState.BLOCKED -> ProcessResult(true, pending = 1)
            NativeMmsAcquisitionState.PENDING -> copyParts(core, provider, tempDirectory, updated, stable)
        }
    }

    private fun mappedPartsRetained(core: MmsCaptureCore, id: String, stable: MmsProviderRecord): Boolean {
        val mapped = try { core.mappedParts(id) } catch (_: Exception) { return false }
        val providerPartIds = stable.parts.mapTo(mutableSetOf()) { it.providerPartId.toString() }
        return mapped.keys.all { it in providerPartIds }
    }

    private fun unavailableUnknownSubscription(
        core: MmsCaptureCore,
        generation: String,
        record: MmsProviderRecord,
        imported: Boolean,
    ): ProcessResult {
        val acquisition = when (val outcome = core.begin(input(generation, UNKNOWN_SUBSCRIPTION, record, imported, inlineText(record)))) {
            is NativeMmsBeginOutcome.Acquired -> outcome.acquisition
            is NativeMmsBeginOutcome.Quarantined -> return ProcessResult(durable = true)
        }
        if (acquisition.state == NativeMmsAcquisitionState.COMPLETE || acquisition.state == NativeMmsAcquisitionState.UNAVAILABLE) {
            return ProcessResult(durable = true)
        }
        core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.UNAVAILABLE, "unknown_subscription")
        return ProcessResult(durable = true, unavailable = 1)
    }

    private fun copyParts(core: MmsCaptureCore, provider: MmsCaptureProvider, tempDirectory: File, acquisition: NativeMmsAcquisition, record: MmsProviderRecord): ProcessResult {
        val existing = try { core.mappedParts(acquisition.acquisitionId) } catch (_: Exception) { return ProcessResult(true, pending = 1) }
        val previewStates = try { core.previewStates(acquisition.acquisitionId) } catch (_: Exception) { return ProcessResult(true, pending = 1) }
        val previewAttempts = PreviewDecodeAttempts(tempDirectory)
        var binary = 0
        var hasDeferredPreview = false
        for (part in ordered(record.parts)) {
            if (part.mediaType.equals("text/plain", true) || part.mediaType.equals(SMIL_MEDIA_TYPE, true)) continue
            if (++binary > MAX_BINARY) {
                core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.BLOCKED, "too_many_attachments")
                return ProcessResult(true, pending = 1)
            }
            val providerPartId = part.providerPartId.toString()
            val previewAttemptKey = "${acquisition.acquisitionId}:${part.providerPartId}"
            val mappedAttachmentId = existing[providerPartId]
            if (mappedAttachmentId != null && previewStates[providerPartId] != NativeMmsPreviewState.DEFERRED) {
                previewAttempts.clear(previewAttemptKey)
                continue
            }
            val remaining = PENDING_MEDIA_LIMIT - core.pendingMediaBytes()
            if (mappedAttachmentId == null && remaining <= 0) {
                core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "media_quota")
                return ProcessResult(true, pending = 1)
            }
            if (mappedAttachmentId != null && remaining < MmsMediaPreview.MAX_JPEG_BYTES) {
                hasDeferredPreview = true
                continue
            }
            val temp = File.createTempFile("mms-", ".part", tempDirectory)
            var attachmentId: String? = null
            var createdAttachment = false
            try {
                provider.copyPart(part, temp, if (mappedAttachmentId == null) min(MAX_BYTES, remaining) else MAX_BYTES)
                attachmentId = mappedAttachmentId ?: core.prepareAttachment(temp.absolutePath, part.mediaType ?: "application/octet-stream", part.name ?: "part-${part.providerPartId}")
                if (mappedAttachmentId == null) {
                    createdAttachment = true
                    core.mapPart(acquisition.acquisitionId, providerPartId, requireNotNull(attachmentId))
                }
                if (!previewAttempts.begin(previewAttemptKey)) {
                    core.setPartPreviewState(acquisition.acquisitionId, providerPartId, NativeMmsPreviewState.PERMANENTLY_UNAVAILABLE)
                    previewAttempts.clear(previewAttemptKey)
                    continue
                }
                val preview = try {
                    provider.inspectMedia(temp, part.mediaType)
                } finally {
                    previewAttempts.finish(previewAttemptKey)
                }
                var thumbnailId: String? = null
                var previewState = when {
                    preview.jpegPreview != null -> NativeMmsPreviewState.READY
                    preview.unavailable == MmsMediaPreviewUnavailable.RESOURCE_LIMIT -> NativeMmsPreviewState.DEFERRED
                    else -> NativeMmsPreviewState.PERMANENTLY_UNAVAILABLE
                }
                preview.jpegPreview?.let { jpeg ->
                    val previewTemp = File.createTempFile("mms-preview-", ".jpg", tempDirectory)
                    try {
                        FileOutputStream(previewTemp).use { it.write(jpeg); it.fd.sync() }
                        if (core.pendingMediaBytes() + jpeg.size <= PENDING_MEDIA_LIMIT) {
                            try {
                                thumbnailId = core.prepareAttachment(previewTemp.absolutePath, "image/jpeg", "preview-${part.providerPartId}.jpg")
                            } catch (_: Exception) {
                                previewState = NativeMmsPreviewState.DEFERRED
                            }
                        } else {
                            previewState = NativeMmsPreviewState.DEFERRED
                        }
                    } finally {
                        previewTemp.delete()
                    }
                }
                try {
                    core.setPartMetadata(
                        acquisition.acquisitionId,
                        providerPartId,
                        preview.metadata.width,
                        preview.metadata.height,
                        preview.metadata.durationMs,
                        thumbnailId,
                    )
                    core.setPartPreviewState(acquisition.acquisitionId, providerPartId, previewState)
                } catch (_: Exception) {
                    thumbnailId?.let { try { core.discardAttachment(it) } catch (_: Exception) { } }
                    core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "preview_metadata_unavailable")
                    return ProcessResult(true, pending = 1)
                }
                if (previewState == NativeMmsPreviewState.DEFERRED) hasDeferredPreview = true
            } catch (error: CancellationException) {
                throw error
            } catch (error: RestrictedMessageAccessRevokedException) {
                throw error
            } catch (_: Exception) {
                if (createdAttachment && attachmentId != null) try { core.discardAttachment(attachmentId) } catch (_: Exception) { }
                if (mappedAttachmentId != null) {
                    try { core.setPartPreviewState(acquisition.acquisitionId, providerPartId, NativeMmsPreviewState.DEFERRED) } catch (_: Exception) { }
                    core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "preview_unavailable")
                } else {
                    core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "part_unavailable")
                }
                return ProcessResult(true, pending = 1, partUnavailable = mappedAttachmentId == null)
            } finally { temp.delete() }
        }
        if (hasDeferredPreview) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "preview_deferred")
            return ProcessResult(true, pending = 1)
        }
        return try { core.complete(acquisition.acquisitionId); ProcessResult(true, captured = 1, verified = true) }
        catch (_: MobileBindingsException.Conflict) {
            ProcessResult(true)
        }
        catch (_: MobileBindingsException.InvalidRequest) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.UNAVAILABLE, "invalid_mms_metadata")
            ProcessResult(true, unavailable = 1)
        }
        catch (_: Exception) {
            core.setState(acquisition.acquisitionId, NativeMmsAcquisitionState.PENDING, "parts_incomplete")
            ProcessResult(true, pending = 1)
        }
    }

    private fun input(generation: String, routeId: String, record: MmsProviderRecord, imported: Boolean, body: String): NativeMmsAcquisitionInput {
        val incoming = record.messageType == RETRIEVE_CONF || record.messageType == NOTIFICATION_IND || record.messageBox == INBOX
        val sender = record.sender.singleOrNull()?.takeIf { it.isNotBlank() }
        // BCC remains local provider provenance. Including it would disclose hidden recipients in group reply-all.
        val recipients = (record.toRecipients + record.ccRecipients).filter { it.isNotBlank() }.distinct()
        return NativeMmsAcquisitionInput(
            NativeMmsSource(generation, routeId, record.id.toString(), record.threadId?.toString()),
            incoming, sender, recipients, record.subject, body, imported,
            record.dateMs ?: UNKNOWN_PROVIDER_DATE_MS, record.transactionId,
        )
    }

    private fun inlineText(record: MmsProviderRecord) = ordered(record.parts)
        .filter { it.mediaType.equals("text/plain", true) }.joinToString("") { it.text.orEmpty() }

    private fun readText(provider: MmsCaptureProvider, record: MmsProviderRecord): String = buildString {
        for (part in ordered(record.parts).filter { it.mediaType.equals("text/plain", true) }) {
            append(provider.readPartText(part, MmsProviderReader.MAX_TEXT) ?: throw MmsProviderException("part text unavailable"))
            if (this.toString().toByteArray().size > MmsProviderReader.MAX_TEXT) throw MmsProviderException("message text exceeds budget")
        }
    }

    private fun ordered(parts: List<MmsProviderPart>) = parts.sortedWith(compareBy<MmsProviderPart> { it.sequence ?: Int.MAX_VALUE }.thenBy { it.providerPartId })

    private fun fingerprint(record: MmsProviderRecord): String {
        val digest = MessageDigest.getInstance("SHA-256")
        fun field(value: Any?) { val bytes = (value?.toString() ?: "<null>").toByteArray(); digest.update(bytes.size.toString().toByteArray()); digest.update(':'.code.toByte()); digest.update(bytes) }
        field(record.id); field(record.threadId); field(record.subscriptionId); field(record.messageBox); field(record.messageType)
        field(record.transactionId); field(record.subject); field(record.expiry); field(record.ready); field(record.error)
        record.sender.forEach(::field); field("to"); record.toRecipients.forEach(::field); field("cc"); record.ccRecipients.forEach(::field); field("bcc"); record.bccRecipients.forEach(::field)
        ordered(record.parts).forEach { field(it.providerPartId); field(it.sequence); field(it.mediaType); field(it.name); field(it.charset); field(it.text) }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun granted(context: Context) =
        ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECEIVE_MMS) == PackageManager.PERMISSION_GRANTED

    internal fun accessFencedProvider(
        provider: MmsCaptureProvider,
        access: () -> RestrictedMessageAccess,
    ): MmsCaptureProvider = object : MmsCaptureProvider by provider {
        private fun <T> read(block: () -> T): T {
            if (access() != RestrictedMessageAccess.ALLOWED) throw RestrictedMessageAccessRevokedException()
            return try {
                val value = block()
                if (access() != RestrictedMessageAccess.ALLOWED) throw RestrictedMessageAccessRevokedException()
                value
            } catch (error: Exception) {
                if (access() != RestrictedMessageAccess.ALLOWED) throw RestrictedMessageAccessRevokedException()
                throw error
            }
        }

        override fun highWater() = read { provider.highWater() }
        override fun page(afterId: Long, limit: Int) = read { provider.page(afterId, limit) }
        override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
            read { provider.descendingPage(cursor, upperBound, floor, limit) }
        override fun read(id: Long) = read { provider.read(id) }
        override fun readPartText(part: MmsProviderPart, byteLimit: Int) = read { provider.readPartText(part, byteLimit) }
        override fun copyPart(part: MmsProviderPart, destination: File, byteLimit: Long) =
            read { provider.copyPart(part, destination, byteLimit) }
    }
    internal fun currentHighWater(context: Context): Long {
        return context.contentResolver.query(Uri.parse("content://mms"), arrayOf("_id"), null, null, "_id DESC LIMIT 1")?.use {
            if (it.moveToFirst()) it.getLong(0) else 0L
        } ?: throw MmsProviderException("MMS provider returned no high-water cursor")
    }

    private const val INBOX = 1
    private const val SENT = 2
    private const val SEND_REQ = 128
    private const val NOTIFICATION_IND = 130
    private const val RETRIEVE_CONF = 132
    private const val SMIL_MEDIA_TYPE = "application/smil"
    private const val UNKNOWN_PROVIDER_DATE_MS = 0L
    private const val UNKNOWN_SUBSCRIPTION = "android-mms-unknown-subscription"

    private enum class ProviderReadKind { HIGH_WATER_OR_PAGE, ROW_READ, PART_TEXT_READ, PART_COPY }

    internal class RestrictedMessageAccessRevokedException : IllegalStateException()

    private data class ProcessResult(
        val durable: Boolean,
        val captured: Int = 0,
        val pending: Int = 0,
        val unavailable: Int = 0,
        val followup: Boolean = false,
        val partUnavailable: Boolean = false,
        val verified: Boolean = false,
    )
    private data class ScanResult(
        val captured: Int,
        val pending: Int,
        val unavailable: Int,
        val more: Boolean,
        val lastDurableId: String?,
        val exhausted: Boolean,
        val rowsQueried: Int,
        val processed: Int,
        val budgetExhausted: Boolean,
    )
    private fun ScanResult.diagnostics() = MmsCaptureScanDiagnostics(
        rowsQueried, processed, captured, pending, unavailable,
        budgetExhausted = budgetExhausted,
    )
}

/** Records only interrupted preview decodes; copied originals remain mapped regardless of outcome. */
internal class PreviewDecodeAttempts(private val directory: File) {
    fun begin(partKey: String): Boolean = safely(true) {
        val attempts = load()
        val key = digest(partKey)
        val interrupted = attempts.getProperty(key)?.toIntOrNull() ?: 0
        if (interrupted >= MAX_INTERRUPTED_DECODES) return@safely false
        if (!attempts.containsKey(key) && attempts.size >= MAX_TRACKED_PARTS) return@safely true
        attempts.setProperty(key, (interrupted + 1).toString())
        save(attempts)
        true
    }

    fun finish(partKey: String) = clear(partKey)

    fun clear(partKey: String) {
        safely(Unit) {
            val attempts = load()
            if (attempts.remove(digest(partKey)) != null) save(attempts)
        }
    }

    private fun load(): Properties = Properties().also { attempts ->
        try {
            if (state.exists()) state.inputStream().use(attempts::load)
        } catch (_: Exception) {
            state.delete()
            attempts.clear()
        }
    }

    private fun save(attempts: Properties) {
        directory.mkdirs()
        val temporary = File(directory, ".${state.name}.part")
        FileOutputStream(temporary).use { output ->
            attempts.store(output, null)
            output.fd.sync()
        }
        if (!temporary.renameTo(state)) throw IOException("preview attempt state write failed")
    }

    private fun <T> safely(default: T, block: () -> T): T = synchronized(LOCK) {
        try { block() } catch (_: Exception) { default }
    }

    private fun digest(value: String): String = MessageDigest.getInstance("SHA-256")
        .digest(value.toByteArray(Charsets.UTF_8))
        .joinToString("") { "%02x".format(it) }

    private val state get() = File(directory, STATE_FILE)

    private companion object {
        const val MAX_INTERRUPTED_DECODES = 3
        const val MAX_TRACKED_PARTS = 256
        const val STATE_FILE = "mms-preview-interrupted-decodes"
        val LOCK = Any()
    }
}

private class NativeCore(internal val client: NativeClientInterface) : MmsCaptureCore {
    override fun begin(input: NativeMmsAcquisitionInput) = client.beginMmsAcquisitionOutcome(input)
    override fun requestManualRecheck(generation: String, scope: String, limit: Int) =
        client.requestMmsRecheck(generation, scope, limit.toULong()).toInt()
    override fun setState(id: String, state: NativeMmsAcquisitionState, reason: String?) = client.setMmsAcquisitionState(id, state, reason)
    override fun migrate(generation: String, scope: String, legacy: MmsLegacyBackfillState) =
        client.migrateMmsBackfillState(generation, scope, NativeMmsLegacyBackfillState(legacy.historyBaseline, legacy.importHistory, legacy.recheckCursor))

    override fun plans(generation: String, scope: String, observedMaxId: String) =
        client.planMmsBackfillScans(generation, scope, observedMaxId).map {
            MmsBackfillScanPlan(
                MmsBackfillPhase.valueOf(it.phase.name),
                it.cursor,
                it.historyUpperBound,
                it.historyComplete,
                it.paused,
                it.budget.coerceAtMost(Int.MAX_VALUE.toULong()).toInt(),
                it.descending,
                it.historyFloor,
                it.providerRescanToken,
            )
        }

    override fun advance(generation: String, scope: String, phase: MmsBackfillPhase, providerId: String) =
        client.advanceMmsBackfillPage(generation, scope, NativeMmsBackfillPhase.valueOf(phase.name), providerId)

    override fun finish(generation: String, scope: String, phase: MmsBackfillPhase, bound: String, exhausted: Boolean) =
        client.finishMmsBackfillPhase(generation, scope, NativeMmsBackfillPhase.valueOf(phase.name), bound, exhausted)

    override fun commitProviderRescanPage(
        generation: String,
        scope: String,
        token: String,
        expectedCursor: String?,
        lastDurableId: String?,
        exhausted: Boolean,
    ) = client.commitMmsProviderRescanPage(generation, scope, token, expectedCursor, lastDurableId, exhausted)

    override fun dueRechecks(generation: String, scope: String, nowMs: Long, conditions: MmsBackfillRecheckConditions): MmsBackfillRecheck {
        return nativeRecheck(client.mmsBackfillDueRechecks(
            generation,
            scope,
            nowMs,
            NativeMmsBackfillRecheckConditions(
                conditions.charging,
                conditions.capacityChanged,
                conditions.explicitRefresh,
                conditions.providerChanged,
            ),
        ))
    }
    private fun nativeRecheck(recheck: uniffi.peppy_mobile_bindings.NativeMmsBackfillRecheck): MmsBackfillRecheck {
        return MmsBackfillRecheck(
            recheck.requests.map {
                MmsBackfillRecheckRequest(
                    providerMessageId = it.providerMessageId,
                    incoming = it.direction == NativeMmsBackfillDirection.INCOMING,
                    acquisitionId = it.acquisitionId,
                    state = MmsBackfillRecheckState.valueOf(it.state.name),
                )
            },
            recheck.budget.coerceAtMost(Int.MAX_VALUE.toULong()).toInt(),
            recheck.continuationDue,
        )
    }
    override fun recordRecheckOutcome(acquisitionId: String, nowMs: Long, outcome: MmsBackfillRecheckOutcome, conditions: MmsBackfillRecheckConditions) {
        client.recordMmsBackfillRecheckOutcome(
            acquisitionId,
            nowMs,
            when (outcome) {
                MmsBackfillRecheckOutcome.VERIFIED -> NativeMmsBackfillRecheckOutcome.VERIFIED
                else -> NativeMmsBackfillRecheckOutcome.valueOf(outcome.name)
            },
            NativeMmsBackfillRecheckConditions(
                conditions.charging,
                conditions.capacityChanged,
                conditions.explicitRefresh,
                conditions.providerChanged,
            ),
        )
    }
    override fun pendingMediaBytes() = client.mmsPendingMediaBytes().toLong()
    override fun prepareAttachment(path: String, mediaType: String, name: String) = client.prepareAttachment(path, mediaType, name).attachmentId
    override fun mapPart(id: String, providerPartId: String, attachmentId: String) = client.setMmsAcquisitionPart(id, providerPartId, attachmentId)
    override fun setPartMetadata(id: String, providerPartId: String, width: Int?, height: Int?, durationMs: Long?, thumbnailAttachmentId: String?) =
        client.setMmsAcquisitionPartMetadata(
            id,
            providerPartId,
            width?.toUInt(),
            height?.toUInt(),
            durationMs?.toULong(),
            thumbnailAttachmentId,
        )
    override fun complete(id: String) { client.completeMmsAcquisition(id) }

    override fun mappedParts(id: String): Map<String, String> =
        client.mmsAcquisitionParts(id).associate { it.providerPartId to it.attachmentId }
    override fun previewStates(id: String): Map<String, NativeMmsPreviewState> =
        client.mmsAcquisitionParts(id).associate { it.providerPartId to it.previewState }
    override fun setPartPreviewState(id: String, providerPartId: String, state: NativeMmsPreviewState) =
        client.setMmsAcquisitionPartPreviewState(id, providerPartId, state)
    override fun discardAttachment(id: String) {
        client.discardUnreferencedAttachment(id)
    }
}
