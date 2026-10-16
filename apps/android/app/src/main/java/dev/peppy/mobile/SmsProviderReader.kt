package dev.peppy.mobile

import android.content.ContentResolver
import android.content.Context
import android.net.Uri

/** A provider row, kept local until the core durably records an acquisition. */
data class SmsProviderRecord(
    val id: Long,
    val threadId: Long?,
    val subscriptionId: Int?,
    val address: String?,
    val body: String?,
    val dateMs: Long?,
    val dateSentMs: Long?,
    val type: Int?,
    val read: Boolean?,
    val seen: Boolean?,
    val status: Int?,
    val creator: String?,
)

data class SmsProviderPage(val records: List<SmsProviderRecord>, val scannedThroughId: Long)

/** Provider failures, including a null cursor, are retryable failures rather than an empty scan. */
class SmsProviderException(message: String, cause: Throwable? = null) : IllegalStateException(message, cause)

/** Read-only, ascending, bounded access to Android's public SMS provider. */
class SmsProviderReader(context: Context) {
    private val resolver: ContentResolver = context.contentResolver

    fun highWater(): Long = query(SMS, arrayOf("_id"), null, null, "_id DESC LIMIT 1") { cursor ->
        if (cursor.moveToFirst()) cursor.long("_id") ?: 0L else 0L
    }

    fun page(afterId: Long, limit: Int): SmsProviderPage {
        require(afterId >= 0)
        val capped = limit.coerceIn(1, MAX_PAGE)
        val records = mutableListOf<SmsProviderRecord>()
        var through = afterId
        query(SMS, COLUMNS, "_id > ?", arrayOf(afterId.toString()), "_id ASC LIMIT $capped") { cursor ->
            while (cursor.moveToNext() && records.size < capped) {
                val id = cursor.long("_id") ?: throw SmsProviderException("provider row has no id")
                if (id <= afterId) continue
                through = id
                records += record(cursor, id)
            }
        }
        records.sortBy { it.id }
        through = records.lastOrNull()?.id ?: afterId
        return SmsProviderPage(records, through)
    }

    fun read(id: Long): SmsProviderRecord? {
        require(id > 0)
        return query(Uri.withAppendedPath(SMS, id.toString()), COLUMNS, null, null, null) { cursor ->
            if (cursor.moveToFirst()) record(cursor, id) else null
        }
    }

    private fun record(cursor: android.database.Cursor, id: Long) = SmsProviderRecord(
        id = id,
        threadId = cursor.long("thread_id"),
        subscriptionId = cursor.long("sub_id")?.toInt()?.takeIf { it >= 0 },
        address = cursor.string("address")?.bounded(),
        body = cursor.string("body")?.bounded(),
        // Android's SMS provider stores milliseconds, unlike the MMS provider's seconds field.
        // Zero (Android writes date_sent=0 for sends) or negative means unknown, never 1970.
        dateMs = cursor.long("date")?.takeIf { it > 0 },
        dateSentMs = cursor.long("date_sent")?.takeIf { it > 0 },
        type = cursor.long("type")?.toInt(),
        read = cursor.long("read")?.let { it != 0L },
        seen = cursor.long("seen")?.let { it != 0L },
        status = cursor.long("status")?.toInt(),
        creator = cursor.string("creator")?.bounded(),
    )

    private fun String.bounded(): String? = takeIf { it.toByteArray(Charsets.UTF_8).size <= MAX_STRING }

    private fun <T> query(
        uri: Uri,
        projection: Array<String>,
        selection: String?,
        selectionArgs: Array<String>?,
        order: String?,
        read: (android.database.Cursor) -> T,
    ): T = try {
        // Only a missing cursor is a provider failure; a null result (row absent) is a valid answer.
        val cursor = resolver.query(uri, projection, selection, selectionArgs, order)
            ?: throw SmsProviderException("SMS provider returned no cursor")
        cursor.use(read)
    } catch (error: SmsProviderException) {
        throw error
    } catch (error: SecurityException) {
        throw SmsProviderException("SMS provider permission denied", error)
    } catch (error: RuntimeException) {
        throw SmsProviderException("SMS provider query failed", error)
    }

    private fun android.database.Cursor.string(name: String): String? =
        getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getString)

    private fun android.database.Cursor.long(name: String): Long? =
        getColumnIndex(name).takeIf { it >= 0 && !isNull(it) }?.let(::getLong)

    companion object {
        private val SMS = Uri.parse("content://sms")
        private val COLUMNS = arrayOf("_id", "thread_id", "sub_id", "address", "body", "date", "date_sent", "type", "read", "seen", "status", "creator")
        const val MAX_PAGE = 50
        const val MAX_STRING = 32 * 1024
    }
}
