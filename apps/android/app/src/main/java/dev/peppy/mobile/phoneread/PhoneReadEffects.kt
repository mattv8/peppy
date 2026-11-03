package dev.peppy.mobile.phoneread

import android.app.KeyguardManager
import android.app.PendingIntent
import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.Telephony
import android.service.notification.StatusBarNotification
import dev.peppy.mobile.NotificationMirrorService
import dev.peppy.mobile.SmsCapture
import dev.peppy.mobile.diagnostics.PhoneReadDiagnostics
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.mms.MmsPreferences
import dev.peppy.mobile.sms.SmsProviderException
import dev.peppy.mobile.sms.SmsProviderReader
import dev.peppy.mobile.sms.SmsProviderRecord
import dev.peppy.mobile.telephony.SimRoutes
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePhoneReadEffect
import uniffi.peppy_mobile_bindings.NativeProviderRowRef

/** Read-only provider view. Any unavailable result means the runner must leave work pending. */
internal interface PhoneReadProvider {
    fun row(id: String): ProviderRowResult
    fun thread(threadId: Long, candidateTimes: Set<Long>): PhoneReadThread?
    fun hasUnreadMms(threadId: Long): Boolean?
    fun collidesWithAnotherThread(threadId: Long, messageTimes: Set<Long>): Boolean?
}

internal data class PhoneReadThread(val thread: ProviderThread, val unreadRows: List<SmsProviderRecord>)
internal sealed interface ProviderRowResult {
    data class Found(val row: SmsProviderRecord) : ProviderRowResult
    data object Missing : ProviderRowResult
    data object Unavailable : ProviderRowResult
}

/** Provider reads are bounded: an incomplete view cannot authorize an action. */
internal class AndroidPhoneReadProvider(context: Context, private val counters: PhoneReadDiagnostics.Counters = PhoneReadDiagnostics.Counters()) : PhoneReadProvider {
    private val resolver: ContentResolver = context.contentResolver

    override fun row(id: String): ProviderRowResult {
        val providerId = id.toLongOrNull()?.takeIf { it > 0 } ?: return ProviderRowResult.Missing
        return try {
            counted { resolver.query(Uri.withAppendedPath(SMS, providerId.toString()), FINGERPRINT_COLUMNS, null, null, null)?.use { cursor ->
                counters.rowsRead += cursor.count
                if (cursor.moveToFirst()) ProviderRowResult.Found(record(cursor, providerId)) else ProviderRowResult.Missing
            } } ?: ProviderRowResult.Unavailable
        } catch (_: SecurityException) { ProviderRowResult.Unavailable } catch (_: RuntimeException) { ProviderRowResult.Unavailable }
    }

    override fun thread(threadId: Long, candidateTimes: Set<Long>): PhoneReadThread? {
        return try {
        val unreadRows: List<SmsProviderRecord> = counted { resolver.query(SMS, FINGERPRINT_COLUMNS, "thread_id=? AND type=? AND read=0", arrayOf(threadId.toString(), SMS_INBOX.toString()), null)?.use { cursor ->
            counters.rowsRead += cursor.count
            buildList<SmsProviderRecord> {
                while (cursor.moveToNext()) {
                    if (size >= MAX_UNREAD_ROWS) throw SmsProviderException("too many unread SMS rows")
                    add(record(cursor, cursor.getLong(cursor.getColumnIndexOrThrow("_id"))))
                }
            }
        } } ?: return null
        if (unreadRows.any { it.dateMs == null || it.type != SMS_INBOX || it.read != false }) return null
        if (unreadRows.isEmpty() || candidateTimes.isEmpty()) {
            return PhoneReadThread(ProviderThread(threadId, unreadRows.map { it.dateMs!! }.toSet(), emptySet()), unreadRows)
        }
        if (candidateTimes.size > MAX_CANDIDATE_TIMES) return null
        val placeholders = candidateTimes.joinToString(",") { "?" }
        val rowDates: Set<Long> = counted { resolver.query(SMS, arrayOf("date"), "thread_id=? AND date IN ($placeholders)", arrayOf(threadId.toString(), *candidateTimes.map(Long::toString).toTypedArray()), null)?.use { cursor ->
            counters.rowsRead += cursor.count
            buildSet<Long> {
                val column = cursor.getColumnIndex("date")
                if (column < 0) throw SmsProviderException("SMS provider date unavailable")
                while (cursor.moveToNext()) {
                    if (cursor.isNull(column)) throw SmsProviderException("SMS provider row incomplete")
                    add(cursor.getLong(column))
                }
            }
        } } ?: return null
        PhoneReadThread(ProviderThread(threadId, unreadRows.map { it.dateMs!! }.toSet(), rowDates), unreadRows)
    } catch (_: SecurityException) { null } catch (_: RuntimeException) { null }
    }

    override fun hasUnreadMms(threadId: Long): Boolean? {
        return try {
            counted { resolver.query(MMS_INBOX, arrayOf("_id"), "thread_id=? AND read=0", arrayOf(threadId.toString()), null)
            ?.use { counters.rowsRead += it.count; it.moveToFirst() } } ?: return null
        } catch (_: SecurityException) { null } catch (_: RuntimeException) { null }
    }

    override fun collidesWithAnotherThread(threadId: Long, messageTimes: Set<Long>): Boolean? {
        if (messageTimes.isEmpty()) return true
        if (messageTimes.size > MAX_COLLISION_TIMES) return null
        return try {
            val byThread = mutableMapOf<Long, MutableSet<Long>>()
            messageTimes.toList().chunked(MAX_IN_ARGS).forEach { chunk ->
                val placeholders = chunk.joinToString(",") { "?" }
                val args = arrayOf(threadId.toString(), *chunk.map(Long::toString).toTypedArray())
                counted { resolver.query(SMS, arrayOf("thread_id", "date"), "thread_id!=? AND date IN ($placeholders)", args, null)?.use { cursor ->
                counters.rowsRead += cursor.count
                val threadColumn = cursor.getColumnIndex("thread_id")
                val dateColumn = cursor.getColumnIndex("date")
                if (threadColumn < 0 || dateColumn < 0) throw SmsProviderException("SMS provider columns unavailable")
                while (cursor.moveToNext()) {
                    if (cursor.isNull(threadColumn) || cursor.isNull(dateColumn)) throw SmsProviderException("SMS provider row incomplete")
                    val otherThread = cursor.getLong(threadColumn)
                    val date = cursor.getLong(dateColumn)
                    if (date in messageTimes) {
                        val dates = byThread.getOrPut(otherThread) { mutableSetOf() }
                        dates += date
                        if (dates.containsAll(messageTimes)) return true
                    }
                }
                } } ?: return null
            }
            false
        } catch (_: SecurityException) { null } catch (_: RuntimeException) { null }
    }

    private companion object {
        val SMS: Uri = Uri.parse("content://sms")
        val MMS_INBOX: Uri = Uri.parse("content://mms/inbox")
        const val SMS_INBOX = 1
        val FINGERPRINT_COLUMNS = arrayOf("_id", "thread_id", "sub_id", "address", "date", "type", "read", "creator")
        const val MAX_IN_ARGS = 900
        const val MAX_COLLISION_TIMES = 10_000
        const val MAX_UNREAD_ROWS = 500
        const val MAX_CANDIDATE_TIMES = 500
    }

    /** Counts one provider query and its wall time; never changes the query's result or exceptions. */
    private inline fun <T> counted(query: () -> T): T {
        val startedNs = System.nanoTime()
        counters.providerQueries++
        try {
            return query()
        } finally {
            counters.providerDurationMs += (System.nanoTime() - startedNs) / 1_000_000L
        }
    }

    private fun record(cursor: android.database.Cursor, id: Long) = SmsProviderRecord(
        id = id,
        threadId = cursor.long("thread_id"),
        subscriptionId = cursor.long("sub_id")?.toInt()?.takeIf { it >= 0 },
        address = cursor.string("address")?.bounded(), body = null,
        dateMs = cursor.long("date")?.takeIf { it > 0 }, dateSentMs = null,
        type = cursor.long("type")?.toInt(), read = cursor.long("read")?.let { it != 0L },
        seen = cursor.long("seen")?.let { it != 0L }, status = cursor.long("status")?.toInt(), creator = cursor.string("creator")?.bounded(),
    )
    private fun android.database.Cursor.long(name: String): Long? = getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getLong)
    private fun android.database.Cursor.string(name: String): String? = getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getString)
    private fun String.bounded(): String? = takeIf { it.toByteArray(Charsets.UTF_8).size <= SmsProviderReader.MAX_STRING }
}

internal interface PhoneReadNotifications {
    fun defaultSmsPackage(): String?
    fun active(packageName: String): List<StatusBarNotification>?
    fun current(key: String, postTime: Long): StatusBarNotification?
    /** Unknown lock state fails closed. */
    fun deviceUnlocked(): Boolean
}

internal interface PhoneReadCore {
    fun allowed(): Boolean
    fun pending(nowMs: Long): List<NativePhoneReadEffect>
    fun complete(ids: List<String>)
    fun rowsSeen(rows: List<NativeProviderRowRef>): Boolean
    fun consume(ids: List<String>, rows: List<NativeProviderRowRef>, nowMs: Long): Int
}

internal class PhoneReadEffects(
    private val context: Context,
    private val core: PhoneReadCore,
    private val provider: PhoneReadProvider,
    private val notifications: PhoneReadNotifications,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val counters: PhoneReadDiagnostics.Counters = PhoneReadDiagnostics.Counters(),
) {
    /** Emits exactly one numeric pass summary for every run, whatever the outcome. */
    fun run(): PhoneReadRun = evaluate().also { PhoneReadDiagnostics.pass(it.outcome, counters) }

    private fun evaluate(): PhoneReadRun {
        if (!core.allowed()) return PhoneReadRun(outcome = PhoneReadDiagnostics.BLOCKED)
        val pending = runCatching { core.pending(nowMs()) }.getOrElse { return PhoneReadRun(outcome = PhoneReadDiagnostics.ERROR) }
        var evaluatedNotifications: Boolean
        var acted = false
        var rowUnavailable = false
        var outcome = PhoneReadDiagnostics.SKIPPED_NO_CANDIDATE
        val stale = mutableListOf<String>()
        val groups = linkedMapOf<Long, MutableList<NativePhoneReadEffect>>()
        for (effect in pending) {
            val row = effect.row
            val current = provider.row(row.providerMessageId)
            if (current == ProviderRowResult.Unavailable) {
                rowUnavailable = true
                outcome = mostSignificant(outcome, PhoneReadDiagnostics.BLOCKED)
                continue
            }
            if (!matchesCurrentSource(row) || (current as? ProviderRowResult.Found)?.row?.let { currentRow ->
                    currentRow.type == SMS_INBOX && currentRow.dateMs == row.providerDateMs &&
                        currentRow.threadId?.toString() == row.providerThreadId &&
                        (currentRow.subscriptionId?.let(SimRoutes::routeId) ?: SmsCapture.UNKNOWN_SUBSCRIPTION) == row.subscriptionId &&
                        SmsCapture.sourceRowFingerprint(currentRow) == row.sourceRowFingerprint
                } != true) {
                stale += effect.messageId
            } else {
                groups.getOrPut(row.providerThreadId.toLongOrNull() ?: -1L) { mutableListOf() } += effect
            }
        }
        if (stale.isNotEmpty()) { acted = true; outcome = mostSignificant(outcome, PhoneReadDiagnostics.COMPLETED_ONLY) }
        complete(stale)
        evaluatedNotifications = groups.isNotEmpty() && !rowUnavailable
        for ((threadId, effects) in groups) {
            if (threadId <= 0) {
                complete(effects.map { it.messageId }); acted = true
                outcome = mostSignificant(outcome, PhoneReadDiagnostics.COMPLETED_ONLY)
                evaluatedNotifications = false
                continue
            }
            val result = runThread(threadId, effects)
            evaluatedNotifications = evaluatedNotifications && result.evaluatedNotifications
            acted = acted || result.acted
            outcome = mostSignificant(outcome, result.outcome)
        }
        return PhoneReadRun(pending.isNotEmpty(), evaluatedNotifications, acted, outcome)
    }

    private fun runThread(threadId: Long, effects: List<NativePhoneReadEffect>): PhoneReadRun {
        if (!core.allowed()) return threadRun(PhoneReadDiagnostics.BLOCKED)
        val defaultPackage = notifications.defaultSmsPackage()
        val active = defaultPackage?.let(notifications::active)
        val facts = active?.map(PhoneReadMatcher::facts).orEmpty()
        val candidateTimes = PhoneReadMatcher.candidateTimes(facts)
        val snapshot = provider.thread(threadId, candidateTimes) ?: return threadRun(PhoneReadDiagnostics.BLOCKED)
        if (snapshot.thread.unreadDates.isEmpty()) {
            complete(effects.map { it.messageId })
            return threadRun(PhoneReadDiagnostics.COMPLETED_ONLY, acted = true)
        }
        if (defaultPackage == null || active == null) return threadRun(PhoneReadDiagnostics.BLOCKED)
        if (candidateTimes.isEmpty()) return threadRun(PhoneReadDiagnostics.SKIPPED_NO_CANDIDATE, evaluated = true)
        when (provider.hasUnreadMms(threadId)) {
            true -> return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true)
            null -> return threadRun(PhoneReadDiagnostics.BLOCKED)
            false -> Unit
        }
        val rows = snapshot.unreadRows.map(::rowRef)
        if (rows.isEmpty() || rows.any { it == null }) return threadRun(PhoneReadDiagnostics.BLOCKED)
        val providerRows = rows.filterNotNull()
        val rowsSeen = try {
            core.rowsSeen(providerRows)
        } catch (_: RuntimeException) {
            return threadRun(PhoneReadDiagnostics.ERROR)
        }
        if (!rowsSeen) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true)
        val ready = PhoneReadMatcher.match(snapshot.thread, facts) as? ReadMatch.Ready ?: return threadRun(PhoneReadDiagnostics.NO_MATCH, evaluated = true)
        when (provider.collidesWithAnotherThread(threadId, ready.notification.messageTimes)) {
            true -> return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true)
            null -> return threadRun(PhoneReadDiagnostics.BLOCKED)
            false -> Unit
        }

        // Consume the durable one-use authorization before the OS boundary. Any outcome after this
        // point, including cancellation or process death, is intentionally never retried.
        val ids = effects.map { it.messageId }
        if (ids.distinct().size != ids.size) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true)
        if (ready.action.requiresUnlockedDevice && !notifications.deviceUnlocked()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true)
        val consumed = try {
            core.consume(ids, providerRows, nowMs())
        } catch (_: RuntimeException) {
            return threadRun(PhoneReadDiagnostics.ERROR)
        }
        if (consumed != ids.size) return threadRun(PhoneReadDiagnostics.BLOCKED)
        if (!core.allowed()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val currentPackage = notifications.defaultSmsPackage() ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        if (currentPackage != defaultPackage) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val currentNotifications = notifications.active(currentPackage) ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val freshCandidateTimes = PhoneReadMatcher.candidateTimes(currentNotifications.map(PhoneReadMatcher::facts))
        val fresh = provider.thread(threadId, freshCandidateTimes) ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val freshRows = fresh.unreadRows.map(::rowRef)
        if (freshRows.any { it == null } || freshRows.filterNotNull().toSet() != providerRows.toSet() ||
            fresh.thread.unreadDates != snapshot.thread.unreadDates || provider.hasUnreadMms(threadId) != false ||
            !core.allowed()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val rematch = PhoneReadMatcher.match(fresh.thread, currentNotifications.map(PhoneReadMatcher::facts)) as? ReadMatch.Ready ?: return threadRun(PhoneReadDiagnostics.NO_MATCH, evaluated = true, acted = true)
        if (rematch.notification.key != ready.notification.key || rematch.notification.postTime != ready.notification.postTime ||
            rematch.action.source != ready.action.source || rematch.action.index != ready.action.index ||
            provider.collidesWithAnotherThread(threadId, rematch.notification.messageTimes) != false || !core.allowed()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val current = notifications.current(rematch.notification.key, rematch.notification.postTime) ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        if (notifications.defaultSmsPackage() != defaultPackage || current.packageName != defaultPackage || !core.allowed()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val currentMatch = PhoneReadMatcher.match(fresh.thread, listOf(PhoneReadMatcher.facts(current))) as? ReadMatch.Ready ?: return threadRun(PhoneReadDiagnostics.NO_MATCH, evaluated = true, acted = true)
        if (currentMatch.notification.key != rematch.notification.key || currentMatch.notification.postTime != rematch.notification.postTime ||
            currentMatch.action.source != rematch.action.source || currentMatch.action.index != rematch.action.index) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val action = PhoneReadMatcher.actionAt(current.notification, currentMatch.action.source, currentMatch.action.index) ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        val intent = action.actionIntent ?: return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        if (currentMatch.action.requiresUnlockedDevice && !notifications.deviceUnlocked()) return threadRun(PhoneReadDiagnostics.BLOCKED, evaluated = true, acted = true)
        try {
            intent.send()
        } catch (_: PendingIntent.CanceledException) {
            // The consumed permit is terminal even when Android cancels the action.
        }
        return threadRun(PhoneReadDiagnostics.ACTED, evaluated = true, acted = true)
    }

    private fun threadRun(outcome: Long, evaluated: Boolean = false, acted: Boolean = false) =
        PhoneReadRun(evaluatedNotifications = evaluated, acted = acted, outcome = outcome)

    /** Most significant outcome wins when one run covers several threads. */
    private fun mostSignificant(current: Long, candidate: Long): Long =
        if (significance(candidate) > significance(current)) candidate else current

    private fun significance(outcome: Long): Int = when (outcome) {
        PhoneReadDiagnostics.ACTED -> 6
        PhoneReadDiagnostics.COMPLETED_ONLY -> 5
        PhoneReadDiagnostics.ERROR -> 4
        PhoneReadDiagnostics.BLOCKED -> 3
        PhoneReadDiagnostics.NO_MATCH -> 2
        else -> 1
    }

    private fun matchesCurrentSource(row: NativeProviderRowRef): Boolean =
        row.sourceGeneration == MmsPreferences(context).sourceGeneration &&
            row.subscriptionId.isNotEmpty() && row.providerMessageId.isNotEmpty() &&
            row.providerThreadId.isNotEmpty() && row.sourceRowFingerprint.isNotEmpty()

    private fun rowRef(row: SmsProviderRecord): NativeProviderRowRef? {
        val threadId = row.threadId ?: return null
        val date = row.dateMs ?: return null
        return NativeProviderRowRef(
            MmsPreferences(context).sourceGeneration,
            row.subscriptionId?.let(SimRoutes::routeId) ?: SmsCapture.UNKNOWN_SUBSCRIPTION,
            row.id.toString(), threadId.toString(), date, SmsCapture.sourceRowFingerprint(row),
        )
    }

    private fun complete(ids: List<String>) {
        if (ids.isEmpty()) return
        runCatching { core.complete(ids.distinct()) }
    }

    private companion object { const val SMS_INBOX = 1 }
}

internal data class PhoneReadRun(
    val pending: Boolean = false,
    val evaluatedNotifications: Boolean = false,
    val acted: Boolean = false,
    val outcome: Long = PhoneReadDiagnostics.SKIPPED_NO_CANDIDATE,
)

internal class NativePhoneReadCore(private val context: Context, private val client: NativeClientInterface) : PhoneReadCore {
    override fun allowed(): Boolean = GatewayPolicyHost(context).decision()?.applyPhoneRead == true
    override fun pending(nowMs: Long) = client.pendingPhoneReadEffects(nowMs, 100u)
    override fun complete(ids: List<String>) = client.completePhoneReadEffects(ids)
    override fun rowsSeen(rows: List<NativeProviderRowRef>) = client.smsProviderRowsSeen(rows)
    override fun consume(ids: List<String>, rows: List<NativeProviderRowRef>, nowMs: Long) = client.consumePhoneReadEffects(ids, rows, nowMs).toInt()
}

internal class AndroidPhoneReadNotifications(private val service: NotificationMirrorService) : PhoneReadNotifications {
    override fun defaultSmsPackage(): String? = runCatching { Telephony.Sms.getDefaultSmsPackage(service) }.getOrNull()
    override fun active(packageName: String): List<StatusBarNotification>? = try {
        service.currentNotifications()?.filter { it.packageName == packageName }
    } catch (_: SecurityException) { null }
    override fun current(key: String, postTime: Long): StatusBarNotification? = try {
        service.currentNotifications()?.firstOrNull { it.key == key && it.postTime == postTime }
    } catch (_: SecurityException) { null }
    override fun deviceUnlocked(): Boolean = service.getSystemService(KeyguardManager::class.java)?.isDeviceLocked == false
}
