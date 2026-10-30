package dev.peppy.mobile

import android.app.PendingIntent
import android.app.KeyguardManager
import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.Telephony
import android.service.notification.StatusBarNotification
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePhoneReadEffect
import uniffi.peppy_mobile_bindings.NativeProviderRowRef

/** Read-only provider view. Any unavailable result means the runner must leave work pending. */
internal interface PhoneReadProvider {
    fun row(id: String): ProviderRowResult
    fun thread(threadId: Long): PhoneReadThread?
    fun hasUnreadMms(threadId: Long): Boolean?
    fun collidesWithAnotherThread(threadId: Long, messageTimes: Set<Long>): Boolean?
}

internal data class PhoneReadThread(val thread: ProviderThread, val unreadRows: List<SmsProviderRecord>)
internal sealed interface ProviderRowResult {
    data class Found(val row: SmsProviderRecord) : ProviderRowResult
    data object Missing : ProviderRowResult
    data object Unavailable : ProviderRowResult
}

/** Provider reads intentionally have no row cap: an incomplete view cannot authorize an action. */
internal class AndroidPhoneReadProvider(context: Context) : PhoneReadProvider {
    private val resolver: ContentResolver = context.contentResolver

    override fun row(id: String): ProviderRowResult {
        val providerId = id.toLongOrNull()?.takeIf { it > 0 } ?: return ProviderRowResult.Missing
        return try {
            resolver.query(Uri.withAppendedPath(SMS, providerId.toString()), FINGERPRINT_COLUMNS, null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) ProviderRowResult.Found(record(cursor, providerId)) else ProviderRowResult.Missing
            } ?: ProviderRowResult.Unavailable
        } catch (_: SecurityException) { ProviderRowResult.Unavailable } catch (_: RuntimeException) { ProviderRowResult.Unavailable }
    }

    override fun thread(threadId: Long): PhoneReadThread? {
        return try {
        val unreadRows: List<SmsProviderRecord> = resolver.query(SMS, FINGERPRINT_COLUMNS, "thread_id=? AND type=? AND read=0", arrayOf(threadId.toString(), SMS_INBOX.toString()), null)?.use { cursor ->
            buildList<SmsProviderRecord> { while (cursor.moveToNext()) add(record(cursor, cursor.getLong(cursor.getColumnIndexOrThrow("_id")))) }
        } ?: return null
        if (unreadRows.any { it.dateMs == null || it.type != SMS_INBOX || it.read != false }) return null
        val rowDates: Set<Long> = resolver.query(SMS, arrayOf("date"), "thread_id=?", arrayOf(threadId.toString()), null)?.use { cursor ->
            buildSet<Long> {
                val column = cursor.getColumnIndex("date")
                if (column < 0) throw SmsProviderException("SMS provider date unavailable")
                while (cursor.moveToNext()) {
                    if (cursor.isNull(column)) throw SmsProviderException("SMS provider row incomplete")
                    add(cursor.getLong(column))
                }
            }
        } ?: return null
        PhoneReadThread(ProviderThread(threadId, unreadRows.map { it.dateMs!! }.toSet(), rowDates), unreadRows)
    } catch (_: SecurityException) { null } catch (_: RuntimeException) { null }
    }

    override fun hasUnreadMms(threadId: Long): Boolean? {
        return try {
            resolver.query(MMS_INBOX, arrayOf("_id"), "thread_id=? AND read=0", arrayOf(threadId.toString()), null)
            ?.use { it.moveToFirst() } ?: return null
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
                resolver.query(SMS, arrayOf("thread_id", "date"), "thread_id!=? AND date IN ($placeholders)", args, null)?.use { cursor ->
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
                } ?: return null
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
) {
    fun run(): Boolean {
        if (!core.allowed()) return false
        val pending = runCatching { core.pending(nowMs()) }.getOrElse { return false }
        val stale = mutableListOf<String>()
        val groups = linkedMapOf<Long, MutableList<NativePhoneReadEffect>>()
        for (effect in pending) {
            val row = effect.row
            val current = provider.row(row.providerMessageId)
            if (current == ProviderRowResult.Unavailable) continue
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
        complete(stale)
        for ((threadId, effects) in groups) {
            if (threadId <= 0) {
                complete(effects.map { it.messageId })
                continue
            }
            runThread(threadId, effects)
        }
        return pending.isNotEmpty()
    }

    private fun runThread(threadId: Long, effects: List<NativePhoneReadEffect>) {
        if (!core.allowed()) return
        val snapshot = provider.thread(threadId) ?: return
        if (snapshot.thread.unreadDates.isEmpty()) {
            complete(effects.map { it.messageId })
            return
        }
        if (provider.hasUnreadMms(threadId) != false) return
        val rows = snapshot.unreadRows.map(::rowRef)
        if (rows.isEmpty() || rows.any { it == null }) return
        val providerRows = rows.filterNotNull()
        val rowsSeen = try {
            core.rowsSeen(providerRows)
        } catch (_: RuntimeException) {
            return
        }
        if (!rowsSeen) return
        val defaultPackage = notifications.defaultSmsPackage() ?: return
        val active = notifications.active(defaultPackage) ?: return
        val facts = active.map(PhoneReadMatcher::facts)
        val ready = PhoneReadMatcher.match(snapshot.thread, facts) as? ReadMatch.Ready ?: return
        if (provider.collidesWithAnotherThread(threadId, ready.notification.messageTimes) != false) return

        // Consume the durable one-use authorization before the OS boundary. Any outcome after this
        // point, including cancellation or process death, is intentionally never retried.
        val ids = effects.map { it.messageId }
        if (ids.distinct().size != ids.size) return
        if (ready.action.requiresUnlockedDevice && !notifications.deviceUnlocked()) return
        val consumed = try {
            core.consume(ids, providerRows, nowMs())
        } catch (_: RuntimeException) {
            return
        }
        if (consumed != ids.size) return
        if (!core.allowed()) return
        val currentPackage = notifications.defaultSmsPackage() ?: return
        if (currentPackage != defaultPackage) return
        val fresh = provider.thread(threadId) ?: return
        val freshRows = fresh.unreadRows.map(::rowRef)
        if (freshRows.any { it == null } || freshRows.filterNotNull().toSet() != providerRows.toSet() ||
            fresh.thread.unreadDates != snapshot.thread.unreadDates || provider.hasUnreadMms(threadId) != false ||
            !core.allowed()) return
        val currentNotifications = notifications.active(currentPackage) ?: return
        val rematch = PhoneReadMatcher.match(fresh.thread, currentNotifications.map(PhoneReadMatcher::facts)) as? ReadMatch.Ready ?: return
        if (rematch.notification.key != ready.notification.key || rematch.notification.postTime != ready.notification.postTime ||
            rematch.action.source != ready.action.source || rematch.action.index != ready.action.index ||
            provider.collidesWithAnotherThread(threadId, rematch.notification.messageTimes) != false || !core.allowed()) return
        val current = notifications.current(rematch.notification.key, rematch.notification.postTime) ?: return
        if (notifications.defaultSmsPackage() != defaultPackage || current.packageName != defaultPackage || !core.allowed()) return
        val currentMatch = PhoneReadMatcher.match(fresh.thread, listOf(PhoneReadMatcher.facts(current))) as? ReadMatch.Ready ?: return
        if (currentMatch.notification.key != rematch.notification.key || currentMatch.notification.postTime != rematch.notification.postTime ||
            currentMatch.action.source != rematch.action.source || currentMatch.action.index != rematch.action.index) return
        val action = PhoneReadMatcher.actionAt(current.notification, currentMatch.action.source, currentMatch.action.index) ?: return
        val intent = action.actionIntent ?: return
        if (currentMatch.action.requiresUnlockedDevice && !notifications.deviceUnlocked()) return
        try {
            intent.send()
        } catch (_: PendingIntent.CanceledException) {
            // The consumed permit is terminal even when Android cancels the action.
        }
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
