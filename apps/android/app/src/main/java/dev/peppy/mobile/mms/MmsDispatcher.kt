package dev.peppy.mobile.mms

import android.app.PendingIntent
import android.content.Context
import android.net.Uri
import androidx.core.content.FileProvider
import dev.peppy.mobile.MmsCallbackIntents
import dev.peppy.mobile.MmsCallbackReconciler
import dev.peppy.mobile.MmsCallbackStore
import dev.peppy.mobile.mms.pdu.MmsFilePart
import dev.peppy.mobile.mms.pdu.MmsPduComposer
import dev.peppy.mobile.mms.pdu.MmsPduException
import dev.peppy.mobile.mms.pdu.MmsPduRequest
import dev.peppy.mobile.mms.pdu.MmsPduValidationException
import dev.peppy.mobile.mms.pdu.MmsRecipient
import dev.peppy.mobile.mms.pdu.MmsRecipientKind
import dev.peppy.mobile.sms.DispatchSummary
import dev.peppy.mobile.sms.SmsDispatcher
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import dev.peppy.mobile.telephony.subscriptionSmsManager
import java.io.File
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSendResult

interface MmsCarrier { fun send(subscriptionId: Int, pdu: Uri, sent: PendingIntent) }

class AndroidMmsCarrier(private val context: Context) : MmsCarrier {
    override fun send(subscriptionId: Int, pdu: Uri, sent: PendingIntent) {
        val manager = subscriptionSmsManager(context, subscriptionId)
        manager.sendMultimediaMessage(context, pdu, null, null, sent)
    }
}

/** Explicit-MMS carrier dispatch; all preflight work occurs before a durable permit. */
class MmsDispatcher(
    private val context: Context, private val client: NativeClientInterface, private val carrier: MmsCarrier,
    private val routes: List<SimRoute>, private val sendPermissionGranted: Boolean, private val enabled: Boolean,
    private val limits: (Int) -> MmsLimits = { MmsLimits.forSubscription(context, it) },
    private val callbacks: MmsCallbackStore = MmsCallbackStore(context),
    private val currentRoutes: () -> List<SimRoute> = { routes },
    private val mayIssueEffect: () -> Boolean = { true },
) {
    fun dispatch(limit: Int = SmsDispatcher.MAX_COMMANDS_PER_RUN): DispatchSummary = synchronized(DISPATCH_LOCK) {
        var submitted = 0; var refused = 0; var waiting = 0; var permits = 0; var fitPreparations = 0; var more = false
        val blockedThreads = mutableSetOf<String>()
        fun waitForEarlier(thread: String?) { waiting++; thread?.let(blockedThreads::add) }
        for (pending in client.pendingCommands()) {
            if (!mayIssueEffect()) break
            if (pending.message.transport != "mms") continue
            if (permits >= limit) { more = true; break }
            val route = SimRoutes.resolve(pending.subscriptionId, currentRoutes())
            val thread = route?.let { "${it.subscriptionId}:${pending.message.recipients.sorted().joinToString(",")}" }
            if (thread != null && thread in blockedThreads) { waiting++; continue }
            if (!enabled || !sendPermissionGranted || route == null) { waitForEarlier(thread); continue }
            val cap = limits(route.subscriptionId)
            if (!cap.enabled || pending.message.recipients.size !in 1..cap.maximumRecipients ||
                (pending.message.body.isEmpty() && pending.message.attachmentIds.isEmpty())) {
                if (refuse(pending.commandId, pending.subscriptionId, route)) { refused++; permits++ } else waiting++
                continue
            }
            val directory = File(context.filesDir, "mms-pdu")
            if ((!directory.exists() && !directory.mkdirs()) || !directory.isDirectory || callbacks.contains(pending.commandId) || !callbacks.hasBudget(cap.maximumPduBytes)) {
                waitForEarlier(thread); continue
            }
            val output = File(directory, "${pending.commandId}.pdu")
            var fittingThisCommand = false
            var fitPreparationBudgetExhausted = false
            val prepared = try {
                compose(pending.commandId, pending.message, cap, output) {
                    if (fittingThisCommand) true
                    else if (fitPreparations >= limit) {
                        fitPreparationBudgetExhausted = true
                        false
                    } else {
                        fittingThisCommand = true
                        fitPreparations++
                        true
                    }
                }
            }
            catch (error: MmsPduValidationException) {
                callbacks.deletePrepared(pending.commandId)
                if (error.message == "output directory does not exist" || error.message == "part is not a regular file") {
                    waitForEarlier(thread)
                } else if (refuse(pending.commandId, pending.subscriptionId, route)) {
                    refused++; permits++
                } else waitForEarlier(thread)
                continue
            } catch (_: MmsPduException) { null } catch (_: MobileBindingsException) { null }
            if (prepared == null) {
                more = more || fitPreparationBudgetExhausted
                waitForEarlier(thread)
                continue
            } // transient media/IO failure: no permit
            val sent = try { MmsCallbackIntents.create(context, pending.commandId) } catch (_: RuntimeException) { callbacks.deletePrepared(pending.commandId); waitForEarlier(thread); continue }
            val uri = try { FileProvider.getUriForFile(context, "${context.packageName}.mms-files", output) }
            catch (_: IllegalArgumentException) { callbacks.deletePrepared(pending.commandId); waitForEarlier(thread); continue }
            // Preparation can take long enough for the default route to change. Do not issue a
            // permit or redirect the prepared PDU to a replacement route.
            if (!mayIssueEffect() || SimRoutes.resolve(pending.subscriptionId, currentRoutes()) != route) {
                callbacks.deletePrepared(pending.commandId)
                waitForEarlier(thread)
                continue
            }
            val permit = client.beginSendAttempt(pending.commandId)
            val command = permit.command
            if (permit.state != NativePermitState.PERMIT || command?.commandId != pending.commandId) { callbacks.deletePrepared(pending.commandId); waitForEarlier(thread); continue }
            permits++
            if (!callbacks.register(command.commandId)) { record(command.commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION); callbacks.deletePrepared(command.commandId); refused++; continue }
            try { carrier.send(route.subscriptionId, uri, sent) }
            catch (_: SecurityException) { failBeforeSubmission(command.commandId); refused++; continue }
            catch (_: IllegalArgumentException) { failBeforeSubmission(command.commandId); refused++; continue }
            catch (_: RuntimeException) { continue }
            record(command.commandId, NativeSendResult.SUBMITTED); submitted++
        }
        return DispatchSummary(submitted, refused, waiting, more)
    }
    private fun compose(commandId: String, message: uniffi.peppy_mobile_bindings.NativeMessage, cap: MmsLimits, output: File, beforePreparation: () -> Boolean): File {
        val handles = mutableListOf<uniffi.peppy_mobile_bindings.NativePlaintextHandle>()
        try {
            message.attachmentIds.forEach { handles += client.openNativePlaintextFile(it) }
            val parts = handles.zip(message.attachmentIds).map { (handle, id) ->
                val info = client.attachmentInfo(id)
                MmsFitPart(File(handle.nativePlaintextPath()), info.mediaType, info.displayName, id)
            }
            fun write(candidate: List<MmsFitPart>, destination: File, maximum: Long = cap.maximumPduBytes): Long = MmsPduComposer.compose(MmsPduRequest(
                "peppy-$commandId", message.recipients.map { MmsRecipient(it, MmsRecipientKind.TO) }, message.subject, message.body,
                candidate.map { MmsFilePart(it.file, it.mediaType, it.name, null) }, maximum, destination,
            )).byteSize
            try {
                write(parts, output)
            } catch (error: MmsPduValidationException) {
                if (error.message != "encoded PDU exceeds maximum size") throw error
                val fitter = MmsImageFitter(File(context.noBackupFilesDir, "mms-fit"), beforePreparation = beforePreparation)
                val fingerprint = MmsImageFitter.fingerprint(parts, cap, "${message.recipients}|${message.subject}|${message.body}")
                when (val fitted = fitter.fit(commandId, fingerprint, cap.maximumPduBytes, parts) { candidate ->
                    val trial = File(output.parentFile, ".mms-fit-$commandId.pdu")
                    try { write(candidate, trial, MmsPduComposer.MAX_PDU_BYTES) } finally { trial.delete() }
                }) {
                    is MmsFitResult.Fitted -> write(fitted.parts, output)
                    MmsFitResult.Impossible -> throw error
                    MmsFitResult.Transient -> throw MmsPduException("image fitting unavailable")
                }
            }
            return output
        } finally { handles.forEach { it.close() } }
    }
    private fun refuse(commandId: String, subscriptionId: String, route: SimRoute): Boolean {
        if (!mayIssueEffect() || SimRoutes.resolve(subscriptionId, currentRoutes()) != route) return false
        val permit = client.beginSendAttempt(commandId)
        if (permit.state != NativePermitState.PERMIT || permit.command?.commandId != commandId) return false
        record(commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION); return true
    }
    private fun failBeforeSubmission(id: String) {
        if (callbacks.record(id, NativeSendResult.FAILED_BEFORE_SUBMISSION)) {
            MmsCallbackReconciler.apply(client, callbacks, id, NativeSendResult.FAILED_BEFORE_SUBMISSION)
        }
    }
    private fun record(id: String, result: NativeSendResult) { try { client.recordSendResult(id, result) } catch (_: MobileBindingsException) {} }
    private companion object { val DISPATCH_LOCK = Any() }
}
