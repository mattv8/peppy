package dev.peppy.mobile.sms

import android.app.PendingIntent
import android.content.Context
import android.telephony.SmsManager
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import dev.peppy.mobile.telephony.subscriptionSmsManager
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSendResult

/** The only carrier boundary. Production uses [AndroidSmsCarrier]; tests record calls. */
interface SmsCarrier {
    fun divide(subscriptionId: Int, body: String): List<String>
    fun send(subscriptionId: Int, destination: String, parts: List<String>, sent: List<PendingIntent>, delivered: List<PendingIntent>)
}

class AndroidSmsCarrier(private val context: Context) : SmsCarrier {
    private fun manager(subscriptionId: Int): SmsManager =
        subscriptionSmsManager(context, subscriptionId)

    override fun divide(subscriptionId: Int, body: String): List<String> {
        val manager = manager(subscriptionId)
        return SmsSegmentation.divide(body) { manager.divideMessage(it) }
    }

    override fun send(subscriptionId: Int, destination: String, parts: List<String>, sent: List<PendingIntent>, delivered: List<PendingIntent>) {
        val manager = manager(subscriptionId)
        if (parts.size == 1) {
            manager.sendTextMessage(destination, null, parts[0], sent[0], delivered[0])
        } else {
            manager.sendMultipartTextMessage(destination, null, ArrayList(parts), ArrayList(sent), ArrayList(delivered))
        }
    }
}

data class DispatchSummary(
    val submitted: Int,
    /** Commands terminally recorded FAILED_BEFORE_SUBMISSION without any OS call. */
    val refusedBeforeCarrier: Int,
    /** Commands left pending: missing permission, no matching live route, or media blocked. */
    val waiting: Int,
    /** The per-run budget stopped the loop with commands still unexamined: schedule another pass. */
    val moreRemaining: Boolean,
    /** Commands blocked specifically because a fresh carrier-only proof has not been supplied. */
    val carrierOnlyProofBlocked: Int = 0,
)

/**
 * Executes pending carrier commands. Per command, strictly in order:
 *  1. transient host preconditions (SEND_SMS granted, exact live route) leave the command pending
 *     with no permit;
 *  2. permanently invalid shapes for this SMS adapter (not exactly one recipient, empty text, any
 *     attachment, more than 255 parts) take the core permit only while that exact route remains
 *     current to record FAILED_BEFORE_SUBMISSION; the OS is never called;
 *  3. the core permit (`beginSendAttempt` == PERMIT) durably records the attempt;
 *  4. callback aggregation is registered with unique PendingIntents;
 *  5. exactly one carrier call; no resend is ever attempted.
 */
class SmsDispatcher(
    private val context: Context,
    private val client: NativeClientInterface,
    private val carrier: SmsCarrier,
    private val routes: List<SimRoute>,
    private val sendPermissionGranted: Boolean,
    private val callbacks: SmsCallbackStore = SmsCallbackStore(context),
    private val currentRoutes: () -> List<SimRoute> = { routes },
    private val mayIssueEffect: () -> Boolean = { true },
) {
    fun dispatch(limit: Int = MAX_COMMANDS_PER_RUN): DispatchSummary {
        var submitted = 0
        var refused = 0
        var waiting = 0
        var carrierOnlyProofBlocked = 0
        var permits = 0
        var more = false
        for (pending in client.pendingCommands()) {
            if (!mayIssueEffect()) break
            // Bound permit/carrier work per run; waiting commands do not starve later ones.
            if (permits >= limit) {
                more = true
                break
            }
            val message = pending.message
            // Explicit MMS commands are owned by MmsDispatcher.  Never consume their permit or
            // reinterpret a single-recipient text-only MMS as SMS.
            if (message.transport != "sms") continue
            val attachments = try {
                client.pendingCommandAttachmentCount(pending.commandId)
            } catch (_: MobileBindingsException.NotFound) {
                continue // no longer pending
            }
            val route = SimRoutes.resolve(pending.subscriptionId, currentRoutes())
            if (!sendPermissionGranted || route == null) {
                waiting++
                continue
            }
            if (message.recipients.size != 1 || message.body.isEmpty() || attachments > 0uL) {
                if (refuseBeforeCarrier(pending.commandId, pending.subscriptionId, route)) {
                    refused++
                    permits++
                } else {
                    waiting++
                }
                continue
            }
            val parts = try {
                carrier.divide(route.subscriptionId, message.body)
            } catch (_: RuntimeException) {
                waiting++
                continue
            }
            if (parts.isEmpty() || parts.size > CallbackIdentity.MAX_PARTS) {
                if (refuseBeforeCarrier(pending.commandId, pending.subscriptionId, route)) {
                    refused++
                    permits++
                } else {
                    waiting++
                }
                continue
            }
            // A route is a current default-SMS route, not a physical-SIM identity. Recheck it
            // immediately before the durable permit so a changed default never authorizes a send.
            if (!mayIssueEffect() || SimRoutes.resolve(pending.subscriptionId, currentRoutes()) != route) {
                waiting++
                continue
            }
            val permit = client.beginSendAttempt(pending.commandId)
            val command = permit.command
            if (permit.state != NativePermitState.PERMIT || command == null || command.commandId != pending.commandId) {
                waiting++
                if (isCarrierOnlyProofBlocked(permit.state)) carrierOnlyProofBlocked++
                continue
            }
            permits++
            val recipient = command.message.recipients.single()
            val sent = parts.indices.map { SmsCallbackIntents.create(context, CallbackIdentity(CallbackKind.SENT, command.commandId, it, parts.size)) }
            val delivered = parts.indices.map { SmsCallbackIntents.create(context, CallbackIdentity(CallbackKind.DELIVERED, command.commandId, it, parts.size)) }
            callbacks.register(command.commandId, parts.size)
            try {
                carrier.send(route.subscriptionId, recipient, parts, sent, delivered)
            } catch (_: SecurityException) {
                record(command.commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION)
                refused++
                continue
            } catch (_: IllegalArgumentException) {
                record(command.commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION)
                refused++
                continue
            } catch (_: RuntimeException) {
                // Unknown platform failure: the core keeps AttemptRecorded (outcome unknown).
                continue
            }
            record(command.commandId, NativeSendResult.SUBMITTED)
            submitted++
        }
        return DispatchSummary(submitted, refused, waiting, more, carrierOnlyProofBlocked)
    }

    /** Takes the permit only to record a truthful terminal refusal; never calls the OS. */
    private fun refuseBeforeCarrier(commandId: String, subscriptionId: String, route: SimRoute): Boolean {
        if (!mayIssueEffect() || SimRoutes.resolve(subscriptionId, currentRoutes()) != route) return false
        val permit = client.beginSendAttempt(commandId)
        if (permit.state != NativePermitState.PERMIT || permit.command?.commandId != commandId) return false
        record(commandId, NativeSendResult.FAILED_BEFORE_SUBMISSION)
        return true
    }

    /** A racing SENT callback may already have advanced the state; the core rejects regressions. */
    private fun record(commandId: String, result: NativeSendResult) {
        try {
            client.recordSendResult(commandId, result)
        } catch (_: MobileBindingsException) {
        }
    }

    companion object {
        const val MAX_COMMANDS_PER_RUN = 10
    }
}

internal fun isCarrierOnlyProofBlocked(state: NativePermitState) =
    state == NativePermitState.CARRIER_ONLY_PROOF_UNAVAILABLE
