package dev.peppy.mobile

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.SystemClock
import android.provider.Telephony
import android.telephony.SmsMessage
import androidx.annotation.VisibleForTesting
import androidx.core.content.ContextCompat
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.NativeSendResult
import java.security.MessageDigest

/** Seam for the one scheduling side effect, so tests can observe capture-before-schedule. */
object GatewayScheduler {
    @VisibleForTesting
    @Volatile
    internal var schedule: (Context) -> Unit = { GatewayWork.enqueueUrgent(it) }
}

/** One PDU's text as decoded by the platform. */
data class IncomingSmsPart(val address: String, val body: String, val timestampMillis: Long)

/**
 * Receives the system SMS_RECEIVED broadcast (sender must hold BROADCAST_SMS, enforced by the
 * manifest). It performs only local durable capture through the core, then schedules bounded
 * sync. No network, no Argon2, no logging of addresses or content.
 */
class IncomingSmsReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val receivedAt = SystemClock.elapsedRealtime()
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return
        val messages: Array<SmsMessage?> = try {
            Telephony.Sms.Intents.getMessagesFromIntent(intent) ?: return
        } catch (_: Exception) {
            return
        }
        val parts = messages.filterNotNull().mapNotNull { message ->
            message.originatingAddress?.let { IncomingSmsPart(it, message.messageBody.orEmpty(), message.timestampMillis) }
        }
        if (parts.isEmpty()) return
        val app = context.applicationContext
        val pending = goAsync()
        ReceiverWork.run(
            receivedAtMillis = receivedAt,
            finish = { pending.finish() },
            recovery = { IncomingCapture.recover(app) },
        ) { IncomingCapture.handle(app, parts) }
    }
}

object IncomingCapture {
    /** Provider recovery is a best-effort hint only; it contains no SMS content or address. */
    fun recover(context: Context) {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) != PackageManager.PERMISSION_GRANTED) {
            ReceiverWork.diagnose("incoming SMS recovery unavailable without READ_SMS")
            return
        }
        try {
            SmsCaptureWork.enqueue(context)
        } catch (_: Exception) {
            ReceiverWork.diagnose("incoming SMS recovery enqueue failed")
        }
    }

    /**
     * Captures every sender's concatenated parts, then schedules sync only if something was
     * durably captured. Returns the number of messages captured (including core duplicates).
     */
    fun handle(context: Context, parts: List<IncomingSmsPart>): Int {
        if (parts.isEmpty() ||
            !GatewayPolicyHost(context).permitsSmsCapture() ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECEIVE_SMS) != PackageManager.PERMISSION_GRANTED
        ) {
            return 0
        }
        // The database opens with the Keystore-wrapped key even while the shared vault is locked;
        // the core stores the capture unsealed and seals it after unlock/key-cache import.
        val client = NativeGateway.open(context) ?: return 0
        var captured = 0
        for ((address, group) in parts.groupBy { it.address }) {
            val body = group.joinToString(separator = "") { it.body }
            val timestamp = group.first().timestampMillis
            try {
                val broadcastId = broadcastId(address, timestamp, body)
                // The SMSC timestamp is the provider's later `date_sent` (reconciliation evidence);
                // users see the local receipt time, matching the provider's `date`.
                client.captureIncomingTimed(
                    NativeIncomingSms(null, address, body, broadcastId, false),
                    timestamp.takeIf { it > 0 },
                    System.currentTimeMillis(),
                )
                captured++
            } catch (_: MobileBindingsException) {
                // Rejected by core validation; nothing durable to sync for this sender.
            }
        }
        if (captured > 0) GatewayScheduler.schedule(context)
        return captured
    }

    /**
     * Deterministic ID so a redelivered broadcast is a core duplicate rather than a second message.
     * Derived from the SMSC timestamp, sender and text; it reveals none of them.
     */
    internal fun broadcastId(address: String, timestampMillis: Long, body: String): String {
        val digest = MessageDigest.getInstance("SHA-256")
            .digest("$address\u0000$timestampMillis\u0000$body".toByteArray(Charsets.UTF_8))
        return "broadcast:" + digest.joinToString("") { "%02x".format(it) }
    }
}

/** Non-exported receiver reached only through this app's explicit callback PendingIntents. */
class SmsStatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val receivedAt = SystemClock.elapsedRealtime()
        val code = resultCode
        val app = context.applicationContext
        val pending = goAsync()
        ReceiverWork.run(
            scheduler = ReceiverWork.evidenceScheduler,
            receivedAtMillis = receivedAt,
            finish = { pending.finish() },
            recovery = { GatewayScheduler.schedule(app) },
        ) {
            record(app, intent, code)?.let { evidence ->
                GatewayScheduler.schedule(app)
                ReceiverWork.submitCallbackApply(recovery = { GatewayScheduler.schedule(app) }, work = {
                    NativeGateway.open(app)?.let { SmsCallbackReconciler.apply(it, evidence.store, evidence.commandId, evidence.result) }
                })
            }
        }
    }

    companion object {
        const val ACTION = "dev.peppy.mobile.SMS_CALLBACK"

        /**
         * Returns the aggregate result accepted by the core, or null when no claim was made (yet).
         * The aggregate is persisted as owed before the core write, so if the database is
         * unavailable or the process dies, the next sync pass replays it.
         */
        internal fun handle(context: Context, intent: Intent, resultCode: Int): NativeSendResult? {
            val recorded = record(context, intent, resultCode) ?: return null
            val accepted = NativeGateway.open(context)?.let { SmsCallbackReconciler.apply(it, recorded.store, recorded.commandId, recorded.result) } == true
            GatewayScheduler.schedule(context)
            return recorded.result.takeIf { accepted }
        }

        private data class Evidence(val store: SmsCallbackStore, val commandId: String, val result: NativeSendResult)

        private fun record(context: Context, intent: Intent, resultCode: Int): Evidence? {
            if (intent.action != ACTION) return null
            val identity = CallbackIdentity.parse(intent.data) ?: return null
            val outcome = when (identity.kind) {
                CallbackKind.SENT -> SmsOutcomes.sentPart(resultCode)
                CallbackKind.DELIVERED -> deliveryOutcome(intent)
            }
            val store = SmsCallbackStore(context)
            val result = store.record(identity, outcome) ?: return null
            return Evidence(store, identity.commandId, result)
        }

        private fun deliveryOutcome(intent: Intent): PartOutcome {
            val format = intent.getStringExtra("format")
            val pdu = intent.getByteArrayExtra("pdu") ?: return PartOutcome.UNKNOWN
            val status = try {
                SmsMessage.createFromPdu(pdu, format)?.status
            } catch (_: RuntimeException) {
                null
            }
            return SmsOutcomes.deliveredPart(format, status)
        }
    }
}
