package dev.peppy.mobile

import android.Manifest
import android.content.ContentResolver
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.content.ContextCompat
import java.util.concurrent.CancellationException
import java.util.concurrent.locks.ReentrantLock
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeGatewayHostFacts
import uniffi.peppy_mobile_bindings.NativeGatewayPlatform
import uniffi.peppy_mobile_bindings.NativePhoneReadCandidate
import uniffi.peppy_mobile_bindings.NativePhoneReadTransport
import uniffi.peppy_mobile_bindings.NativeProviderRowRef

internal enum class PhoneReadTransport { SMS, MMS }

internal data class PhoneReadProviderRow(
    val sourceGeneration: String,
    val subscriptionId: String,
    val providerMessageId: String,
    val providerThreadId: String,
    val providerDateMs: Long,
    val sourceRowFingerprint: String,
)

internal data class PhoneReadCandidate(
    val messageId: String,
    val transport: PhoneReadTransport,
    val localOrder: Long,
    val row: PhoneReadProviderRow,
    val transactionId: String?,
)

internal interface PhoneReadIngestCore {
    fun candidates(transport: PhoneReadTransport, sourceGeneration: String, afterOrder: Long, limit: Int): List<PhoneReadCandidate>
    fun apply(candidates: List<PhoneReadCandidate>): Int
}

/** `Unavailable` retains the cursor so a transient provider failure cannot skip evidence. */
internal enum class PhoneReadProviderResult { READ, UNREAD, MISSING, UNAVAILABLE }

internal interface PhoneReadIngestProvider {
    fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult
}

internal interface PhoneReadIngestPolicy {
    fun allowed(transport: PhoneReadTransport): Boolean
}

internal interface PhoneReadCursorStore {
    fun cursor(transport: PhoneReadTransport, generation: String): Long
    fun save(transport: PhoneReadTransport, generation: String, cursor: Long)
}

/**
 * Bounded, provider-read-only reconciliation. Cursors are merely an optimization: core validates
 * every submitted acquisition again inside its transaction.
 */
internal class PhoneReadIngestRunner(
    private val context: Context,
    private val core: PhoneReadIngestCore,
    private val provider: PhoneReadIngestProvider,
    private val policy: PhoneReadIngestPolicy,
    private val cursors: PhoneReadCursorStore,
    private val generation: () -> String = { MmsPreferences(context).sourceGeneration },
    private val hasReadPermission: () -> Boolean = {
        ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED
    },
) {
    fun run(): Boolean {
        ensureNotCancelled()
        try {
            RUN_LOCK.lockInterruptibly()
        } catch (_: InterruptedException) {
            throw CancellationException("phone read reconciliation cancelled")
        }
        return try {
            val sourceGeneration = generation()
            PhoneReadTransport.entries.fold(false) { applied, transport -> applied or reconcile(transport, sourceGeneration) }
        } finally {
            RUN_LOCK.unlock()
        }
    }

    private fun reconcile(transport: PhoneReadTransport, sourceGeneration: String): Boolean {
        try {
            if (!policy.allowed(transport)) return false
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            return false
        }
        var cursor = cursors.cursor(transport, sourceGeneration).coerceAtLeast(0)
        var applied = false
        // A scan that starts at zero has already covered the whole range; only a nonzero start may wrap once.
        var wrapped = cursor == 0L
        repeat(MAX_PAGES) {
            ensureNotCancelled()
            val page = try {
                core.candidates(transport, sourceGeneration, cursor, PAGE_SIZE)
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                return applied
            }
            if (page.isEmpty()) {
                cursors.save(transport, sourceGeneration, 0)
                if (wrapped) return applied
                cursor = 0
                wrapped = true
                return@repeat
            }
            if (page.size > PAGE_SIZE) return applied
            var advanced = cursor
            for (candidate in page) {
                ensureNotCancelled()
                if (candidate.localOrder <= advanced) continue
                if (!validCandidate(candidate, transport, sourceGeneration)) {
                    advanced = candidate.localOrder
                    continue
                }
                val providerResult = try {
                    provider.isRead(candidate)
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    cursors.save(transport, sourceGeneration, advanced)
                    return applied
                }
                when (providerResult) {
                    PhoneReadProviderResult.UNAVAILABLE -> {
                        cursors.save(transport, sourceGeneration, advanced)
                        return applied
                    }
                    PhoneReadProviderResult.READ -> {
                        // Policy and permission must hold at the mutation boundary, not only at scan start.
                        val allowed = try {
                            hasReadPermission() && policy.allowed(transport)
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            false
                        }
                        if (!allowed) {
                            cursors.save(transport, sourceGeneration, advanced)
                            return applied
                        }
                        val accepted = try {
                            core.apply(listOf(candidate))
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            cursors.save(transport, sourceGeneration, advanced)
                            return applied
                        }
                        if (accepted !in 0..1) {
                            cursors.save(transport, sourceGeneration, advanced)
                            return applied
                        }
                        applied = applied || accepted == 1
                    }
                    PhoneReadProviderResult.UNREAD, PhoneReadProviderResult.MISSING -> Unit
                }
                advanced = candidate.localOrder
            }
            cursor = advanced
            if (page.size < PAGE_SIZE) {
                cursors.save(transport, sourceGeneration, 0)
                if (wrapped) return applied
                cursor = 0
                wrapped = true
                return@repeat
            }
            cursors.save(transport, sourceGeneration, cursor)
        }
        return applied
    }

    private fun validCandidate(candidate: PhoneReadCandidate, transport: PhoneReadTransport, generation: String): Boolean =
        candidate.messageId.isNotEmpty() && candidate.transport == transport &&
            candidate.row.sourceGeneration == generation && candidate.row.subscriptionId.isNotEmpty() &&
            candidate.row.providerMessageId.toLongOrNull()?.let { it > 0 } == true &&
            candidate.row.providerThreadId.toLongOrNull()?.let { it > 0 } == true && candidate.row.providerDateMs > 0 &&
            when (transport) {
                PhoneReadTransport.SMS -> candidate.row.sourceRowFingerprint.isNotEmpty() && candidate.transactionId == null
                PhoneReadTransport.MMS -> candidate.row.sourceRowFingerprint.isEmpty()
            }

    private fun ensureNotCancelled() {
        if (Thread.currentThread().isInterrupted) throw CancellationException("phone read reconciliation cancelled")
    }

    private companion object {
        const val PAGE_SIZE = 100
        const val MAX_PAGES = 5
        val RUN_LOCK = ReentrantLock()
    }
}

/** Entry point for capture and lifecycle workers; a true result asks the host to schedule sync. */
object PhoneReadIngest {
    fun run(context: Context, client: NativeClientInterface): Boolean = PhoneReadIngestRunner(
        context.applicationContext,
        NativePhoneReadIngestCore(client),
        AndroidPhoneReadIngestProvider(context.applicationContext),
        NativePhoneReadIngestPolicy(context.applicationContext, client),
        SharedPreferencesPhoneReadCursorStore(context.applicationContext),
    ).run()
}

private class NativePhoneReadIngestCore(private val client: NativeClientInterface) : PhoneReadIngestCore {
    override fun candidates(transport: PhoneReadTransport, sourceGeneration: String, afterOrder: Long, limit: Int) =
        client.phoneReadCandidates(transport.native(), sourceGeneration, afterOrder, limit.toUInt()).map {
            PhoneReadCandidate(it.messageId, it.transport.kotlin(), it.localOrder, it.row.kotlin(), it.transactionId)
        }

    override fun apply(candidates: List<PhoneReadCandidate>) = client.applyPhoneReads(candidates.map { candidate ->
        NativePhoneReadCandidate(candidate.messageId, candidate.transport.native(), candidate.localOrder, candidate.row.native(), candidate.transactionId)
    }).toInt()
}

internal class NativePhoneReadIngestPolicy(
    private val context: Context,
    private val client: NativeClientInterface,
) : PhoneReadIngestPolicy {
    override fun allowed(transport: PhoneReadTransport): Boolean {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) != PackageManager.PERMISSION_GRANTED) return false
        val notificationAccess = NotificationMirrorAccess.granted(context)
        return try {
            val decision = client.gatewayPolicyDecision(
                NativeGatewayPlatform.ANDROID,
                NativeGatewayHostFacts(
                    GatewayPolicyHost.onWifi(context), notificationAccess, true, false,
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && notificationAccess,
                ),
            )
            decision.ingestPhoneRead && (
                transport == PhoneReadTransport.SMS ||
                    (MmsPreferences(context).enabled && decision.captureMms)
                )
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            false
        }
    }
}

private class SharedPreferencesPhoneReadCursorStore(context: Context) : PhoneReadCursorStore {
    private val preferences = context.getSharedPreferences("peppy-phone-read-ingest", Context.MODE_PRIVATE)
    override fun cursor(transport: PhoneReadTransport, generation: String) = preferences.getLong(key(transport, generation), 0).coerceAtLeast(0)
    override fun save(transport: PhoneReadTransport, generation: String, cursor: Long) {
        preferences.edit().putLong(key(transport, generation), cursor.coerceAtLeast(0)).apply()
    }

    // The Android binding exposes no vault identifier. Source generation is an install-unique
    // acquisition scope, and the core still authenticates all candidate evidence on every pass.
    private fun key(transport: PhoneReadTransport, generation: String) = "${transport.name}:$generation"
}

internal class AndroidPhoneReadIngestProvider(context: Context) : PhoneReadIngestProvider {
    private val resolver: ContentResolver = context.contentResolver

    override fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult = try {
        when (candidate.transport) {
            PhoneReadTransport.SMS -> smsRead(candidate)
            PhoneReadTransport.MMS -> mmsRead(candidate)
        }
    } catch (_: SecurityException) {
        PhoneReadProviderResult.UNAVAILABLE
    } catch (error: CancellationException) {
        throw error
    } catch (_: Exception) {
        PhoneReadProviderResult.UNAVAILABLE
    }

    private fun smsRead(candidate: PhoneReadCandidate): PhoneReadProviderResult = resolver.query(
        Uri.withAppendedPath(SMS, candidate.row.providerMessageId), SMS_COLUMNS, null, null, null,
    )?.use { cursor ->
        if (!cursor.moveToFirst()) return PhoneReadProviderResult.MISSING
        if (!cursor.hasColumns(SMS_COLUMNS) || cursor.long("thread_id") == null || cursor.long("date") == null ||
            cursor.long("type") == null || cursor.long("read") == null) return PhoneReadProviderResult.MISSING
        val row = SmsProviderRecord(
            candidate.row.providerMessageId.toLong(), cursor.long("thread_id"), cursor.long("sub_id")?.toInt()?.takeIf { it >= 0 },
            cursor.string("address"), null, cursor.long("date")?.takeIf { it > 0 }, null, cursor.long("type")?.toInt(),
            cursor.long("read")?.let { it == 1L }, null, null, cursor.string("creator"),
        )
        if (row.type != SMS_INBOX || row.threadId?.toString() != candidate.row.providerThreadId || row.dateMs != candidate.row.providerDateMs ||
            (row.subscriptionId?.let(SimRoutes::routeId) ?: SmsCapture.UNKNOWN_SUBSCRIPTION) != candidate.row.subscriptionId ||
            SmsCapture.sourceRowFingerprint(row) != candidate.row.sourceRowFingerprint) PhoneReadProviderResult.MISSING
        else if (row.read == true) PhoneReadProviderResult.READ else PhoneReadProviderResult.UNREAD
    } ?: PhoneReadProviderResult.UNAVAILABLE

    private fun mmsRead(candidate: PhoneReadCandidate): PhoneReadProviderResult = resolver.query(
        Uri.withAppendedPath(MMS, candidate.row.providerMessageId), MMS_COLUMNS, null, null, null,
    )?.use { cursor ->
        if (!cursor.moveToFirst()) return PhoneReadProviderResult.MISSING
        if (!cursor.hasColumns(MMS_COLUMNS) || cursor.long("thread_id") == null || cursor.long("msg_box") == null ||
            cursor.long("date") == null || cursor.long("read") == null) return PhoneReadProviderResult.MISSING
        val seconds = cursor.long("date") ?: return PhoneReadProviderResult.MISSING
        val dateMs = seconds.takeIf { it in 1..Long.MAX_VALUE / 1_000L }?.times(1_000L) ?: return PhoneReadProviderResult.MISSING
        val subscription = cursor.long("sub_id")?.toInt()?.takeIf { it >= 0 }?.let(SimRoutes::routeId)
            ?: return PhoneReadProviderResult.MISSING
        if (cursor.long("msg_box") != MMS_INBOX || cursor.long("thread_id")?.toString() != candidate.row.providerThreadId ||
            subscription != candidate.row.subscriptionId || dateMs != candidate.row.providerDateMs || cursor.string("tr_id") != candidate.transactionId) {
            PhoneReadProviderResult.MISSING
        } else if (cursor.long("read") == 1L) PhoneReadProviderResult.READ else PhoneReadProviderResult.UNREAD
    } ?: PhoneReadProviderResult.UNAVAILABLE

    private fun android.database.Cursor.long(name: String) = getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getLong)
    private fun android.database.Cursor.string(name: String) = getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getString)
    private fun android.database.Cursor.hasColumns(columns: Array<String>) = columns.all { getColumnIndex(it) >= 0 }

    private companion object {
        val SMS: Uri = Uri.parse("content://sms")
        val MMS: Uri = Uri.parse("content://mms")
        val SMS_COLUMNS = arrayOf("_id", "thread_id", "sub_id", "address", "date", "type", "read", "creator")
        val MMS_COLUMNS = arrayOf("_id", "thread_id", "sub_id", "msg_box", "date", "read", "tr_id")
        const val SMS_INBOX = 1
        const val MMS_INBOX = 1L
    }
}

private fun PhoneReadTransport.native() = if (this == PhoneReadTransport.SMS) NativePhoneReadTransport.SMS else NativePhoneReadTransport.MMS
private fun NativePhoneReadTransport.kotlin() = if (this == NativePhoneReadTransport.SMS) PhoneReadTransport.SMS else PhoneReadTransport.MMS
private fun PhoneReadProviderRow.native() = NativeProviderRowRef(sourceGeneration, subscriptionId, providerMessageId, providerThreadId, providerDateMs, sourceRowFingerprint)
private fun NativeProviderRowRef.kotlin() = PhoneReadProviderRow(sourceGeneration, subscriptionId, providerMessageId, providerThreadId, providerDateMs, sourceRowFingerprint)
