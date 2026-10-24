package dev.peppy.mobile

import android.content.Context
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeSmsAcquisitionState
import uniffi.peppy_mobile_bindings.NativeSmsProviderAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeSmsSource

/**
 * Generated-core bridge for the bounded provider scanner. WorkManager unique work does not
 * serialize the one-time and periodic requests against each other, so [SmsCaptureWork.serialized]
 * (an in-process mutex) guards every scanner pass.
 */
internal object SmsNativeCapture : SmsCaptureRunner {
    override fun run(context: Context): SmsCaptureSummary {
        if (!smsReadGranted(context) || !GatewayPolicyHost(context).permitsSmsCapture()) return SmsCaptureSummary()
        val client = NativeGateway.open(context) ?: return SmsCaptureSummary()
        val summary = capture(client, AndroidSmsCaptureProvider(context), MmsPreferences(context).sourceGeneration)
        if (shouldScheduleGatewayUpload(summary)) {
            if (SyncDiagnostics.enabled) {
                GatewayWork.enqueue(context.applicationContext, diagnosticParent = SyncDiagnostics.currentTraceId())
            } else {
                GatewayWork.enqueue(context.applicationContext)
            }
        }
        return summary
    }

    /** One bounded pass over the live/history plans and, when due, the reconciliation sweep. */
    internal fun capture(client: NativeClientInterface, reader: AndroidSmsCaptureProvider, generation: String): SmsCaptureSummary {
        val scope = SmsCapture.PROVIDER_SCOPE
        reader.beginDiagnosticPass()
        return try {
            val highWater = reader.highWater().toString()
            val plans = client.planSmsScans(generation, scope, highWater)
            val core = NativeSmsCaptureCore(client)
            var recheckPending = true
            var summary = SmsCaptureSummary()
            for (plan in plans) {
                val result = if (recheckPending) {
                    SmsCapture.run(plan.toCapturePlan(), core, reader)
                } else {
                    SmsCapture.runWithoutPendingRecheck(plan.toCapturePlan(), core, reader)
                }
                if (recheckPending) {
                    client.setSmsPendingRecheckCursor(plan.sourceGeneration, plan.subscriptionId, plan.imported, result.pendingRecheckCursor)
                    recheckPending = false
                }
                if (result.historyExhausted) {
                    val bound = requireNotNull(plan.historyUpperBound) { "history exhaustion without frozen bound" }
                    client.ackSmsScanExhausted(plan.sourceGeneration, plan.subscriptionId, plan.imported, bound)
                }
                summary += result
                recordPlanCompletion(if (plan.imported) PLAN_HISTORY else PLAN_LIVE, plan.imported, result)
                if (result.providerError) {
                    client.setSmsProviderError(generation, scope, true)
                    return summary
                }
                if (result.coreError) return summary
            }
            // Completed sweeps restart only after the core's interval unless the core sees reused
            // IDs (high-water below the live checkpoint, or a different row at that checkpoint).
            val anchor = plans.firstOrNull { !it.imported }?.checkpoint?.toLongOrNull()?.takeIf { it > 0 }
                ?.let(reader::read)?.let(SmsCapture::sourceRowFingerprint)
            val reconciliation = client.planSmsReconciliation(generation, scope, highWater, anchor)
            if (reconciliation != null) {
                val result = captureTimed(SyncDiagnosticStage.SMS_RECONCILIATION) {
                    SmsCapture.reconcile(reconciliation.toCapturePlan(), core, reader)
                }
                summary += result.summary
                captureProgress(SyncDiagnosticStage.SMS_RECONCILIATION, result.summary)
                recordPlanCompletion(PLAN_RECONCILIATION, imported = false, result.summary)
                if (result.summary.providerError) {
                    client.setSmsProviderError(generation, scope, true)
                    return summary
                }
                if (result.summary.coreError) return summary
                client.completeSmsReconciliation(
                    reconciliation.sourceGeneration,
                    reconciliation.subscriptionId,
                    reconciliation.upperBound,
                    result.cursor.takeIf { it > 0 }?.toString(),
                    result.exhausted,
                )
            }
            client.setSmsProviderError(generation, scope, false)
            summary
        } catch (_: SmsProviderException) {
            runCatching { client.setSmsProviderError(generation, scope, true) }
            SmsCaptureSummary(more = true, providerError = true)
        } catch (_: MobileBindingsException) {
            // Database/core failure: not an Android provider outage. WorkManager backs off.
            SmsCaptureSummary(coreError = true)
        } finally {
            reader.finishDiagnosticPass()
        }
    }
}

private fun recordPlanCompletion(category: Long, imported: Boolean, summary: SmsCaptureSummary) {
    if (!SyncDiagnostics.enabled) return
    SyncDiagnostics.record(
        SyncTraceEvent.CAPTURE_PLAN,
        SyncTraceField.SOURCE to category,
        SyncTraceField.IMPORTED to if (imported) 1 else 0,
        SyncTraceField.CAPTURED to summary.captured.toLong(),
        SyncTraceField.PENDING to summary.pending.toLong(),
        SyncTraceField.UNAVAILABLE to summary.unavailable.toLong(),
        SyncTraceField.PAUSED to summary.paused.toLong(),
        SyncTraceField.MORE to if (summary.more) 1 else 0,
        SyncTraceField.PROVIDER_ERROR to if (summary.providerError) 1 else 0,
        SyncTraceField.CORE_ERROR to if (summary.coreError) 1 else 0,
    )
}

private const val PLAN_LIVE = 0L
private const val PLAN_HISTORY = 1L
private const val PLAN_RECONCILIATION = 2L

internal fun shouldScheduleGatewayUpload(summary: SmsCaptureSummary): Boolean = summary.captured > 0

internal class NativeSmsCaptureCore(private val client: NativeClientInterface) : SmsCaptureCore {
    override fun acquisitions(afterId: String?, limit: Int): List<SmsAcquisition> =
        client.smsAcquisitionsAfter(afterId, limit.toULong()).map { acquisition ->
            SmsAcquisition(
                acquisition.acquisitionId,
                acquisition.input.toCaptureInput(),
                acquisition.state.toCaptureState(),
                acquisition.reason,
            )
        }

    override fun begin(input: SmsAcquisitionInput): SmsAcquisition {
        val acquisition = try {
            client.beginSmsProviderAcquisition(input.toNativeInput())
        } catch (_: MobileBindingsException.SmsCapacity) {
            throw SmsCaptureCapacityException()
        }
        return SmsAcquisition(
            acquisition.acquisitionId,
            acquisition.input.toCaptureInput(),
            acquisition.state.toCaptureState(),
            acquisition.reason,
        )
    }

    override fun complete(id: String): SmsCompletion = try {
        if (client.completeSmsAcquisition(id).duplicate) SmsCompletion.DUPLICATE else SmsCompletion.CAPTURED
    } catch (_: MobileBindingsException.InvalidRequest) {
        // The core has already recorded the row as terminal (ambiguous or unavailable).
        SmsCompletion.UNAVAILABLE
    }

    override fun setState(id: String, state: SmsAcquisitionState, reason: String?) {
        client.setSmsAcquisitionState(id, state.toNativeState(), reason)
    }

    override fun setCheckpoint(sourceGeneration: String, subscription: String, imported: Boolean, providerId: String) {
        client.setSmsScanCheckpoint(sourceGeneration, subscription, imported, providerId)
    }
}

internal class AndroidSmsCaptureProvider(context: Context) : SmsCaptureProvider {
    private val reader = SmsProviderReader(context)

    fun highWater(): Long = reader.highWater()

    fun beginDiagnosticPass() = reader.beginDiagnosticPass()

    fun finishDiagnosticPass() = reader.finishDiagnosticPass()

    override fun page(afterId: Long, limit: Int) = reader.page(afterId, limit)

    override fun descendingPage(cursor: Long?, upperBound: Long, floor: Long?, limit: Int) =
        reader.descendingPage(cursor, upperBound, floor, limit)

    override fun read(id: Long) = reader.read(id)
}

private fun uniffi.peppy_mobile_bindings.NativeSmsScanPlan.toCapturePlan() = SmsCapturePlan(
    sourceGeneration = sourceGeneration,
    subscriptionId = subscriptionId,
    imported = imported,
    checkpoint = checkpoint?.toLongOrNull() ?: if (descending) null else 0L,
    pendingRecheckCursor = pendingRecheckCursor,
    historyUpperBound = historyUpperBound?.toLongOrNull(),
    historyComplete = historyComplete,
    paused = paused,
    descending = descending,
    historyFloor = historyFloor?.toLongOrNull(),
)

private fun uniffi.peppy_mobile_bindings.NativeSmsReconciliationPlan.toCapturePlan() = SmsReconciliationPlan(
    sourceGeneration,
    subscriptionId,
    cursor?.toLongOrNull() ?: 0L,
    upperBound.toLongOrNull() ?: 0L,
    historyUpperBound?.toLongOrNull(),
)

private fun uniffi.peppy_mobile_bindings.NativeSmsProviderAcquisitionInput.toCaptureInput() = SmsAcquisitionInput(
    source.sourceGeneration,
    source.subscriptionId,
    source.providerMessageId,
    source.providerThreadId,
    incoming,
    senderAddress,
    recipients,
    body,
    imported,
    sentAtMs,
    dateSentMs,
    creator,
    sourceRowFingerprint,
    source.broadcastId,
)

private fun SmsAcquisitionInput.toNativeInput() = NativeSmsProviderAcquisitionInput(
    NativeSmsSource(sourceGeneration, subscriptionId, providerMessageId, providerThreadId, broadcastId),
    incoming,
    senderAddress,
    recipients,
    body,
    imported,
    sentAtMs,
    dateSentMs,
    creator,
    sourceRowFingerprint,
)

private fun NativeSmsAcquisitionState.toCaptureState(): SmsAcquisitionState = when (this) {
    NativeSmsAcquisitionState.PENDING -> SmsAcquisitionState.PENDING
    NativeSmsAcquisitionState.UNAVAILABLE -> SmsAcquisitionState.UNAVAILABLE
    NativeSmsAcquisitionState.COMPLETE -> SmsAcquisitionState.COMPLETE
    NativeSmsAcquisitionState.PAUSED -> SmsAcquisitionState.PAUSED
}

private fun SmsAcquisitionState.toNativeState(): NativeSmsAcquisitionState = when (this) {
    SmsAcquisitionState.PENDING -> NativeSmsAcquisitionState.PENDING
    SmsAcquisitionState.UNAVAILABLE -> NativeSmsAcquisitionState.UNAVAILABLE
    SmsAcquisitionState.COMPLETE -> NativeSmsAcquisitionState.COMPLETE
    SmsAcquisitionState.PAUSED -> NativeSmsAcquisitionState.PAUSED
}

private operator fun SmsCaptureSummary.plus(other: SmsCaptureSummary) = SmsCaptureSummary(
    captured + other.captured,
    pending + other.pending,
    unavailable + other.unavailable,
    paused + other.paused,
    more || other.more,
    providerError || other.providerError,
    other.pendingRecheckCursor ?: pendingRecheckCursor,
    historyExhausted || other.historyExhausted,
    coreError || other.coreError,
    processed + other.processed,
    rowsQueried + other.rowsQueried,
    advanced || other.advanced,
    budgetExhausted || other.budgetExhausted,
)
