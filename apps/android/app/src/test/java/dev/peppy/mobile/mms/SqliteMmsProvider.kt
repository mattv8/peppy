package dev.peppy.mobile.mms

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.sqlite.SQLiteDatabase
import android.net.Uri

/**
 * MMS provider backed by a real SQLite database (Robolectric native SQLite), so a projection naming
 * a column the schema lacks fails with SQLite's own "no such column" exception.
 */
class SqliteMmsProvider : ContentProvider() {
    var database: SQLiteDatabase? = null
    /** Base-URI queries whose projection names an optional column; the production probe. */
    var optionalProbes = 0

    override fun onCreate() = true

    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
        val db = checkNotNull(database) { "database not set" }
        val segments = uri.pathSegments
        if (segments.isEmpty() && projection.orEmpty().any { it in OPTIONAL_COLUMNS }) optionalProbes++
        return when {
            segments.isEmpty() -> db.query(PDU, projection, selection, selectionArgs, null, null, sortOrder)
            segments.first() == "part" -> db.query(PART, projection, selection, selectionArgs, null, null, sortOrder)
            segments.size == 1 -> db.query(PDU, projection, "_id = ?", arrayOf(segments.single()), null, null, null)
            segments.size == 2 && segments[1] == "addr" -> {
                val where = if (selection == null) "msg_id = ?" else "msg_id = ? AND ($selection)"
                db.query(ADDR, projection, where, arrayOf(segments[0]) + selectionArgs.orEmpty(), null, null, null)
            }
            else -> throw IllegalArgumentException("Unsupported URI $uri")
        }
    }

    override fun getType(uri: Uri): String? = null
    override fun insert(uri: Uri, values: ContentValues?): Uri? = null
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0

    companion object {
        private const val PDU = "pdu"
        private const val ADDR = "addr"
        private const val PART = "part"
        private val OPTIONAL_COLUMNS = setOf("restricted", "text_only")
    }
}

/** Creates the MMS tables; [flagColumns] adds the optional `restricted` and `text_only` columns of a modern schema. */
fun createMmsTables(db: SQLiteDatabase, flagColumns: Boolean) {
    val flags = if (flagColumns) ", restricted INTEGER, text_only INTEGER" else ""
    db.execSQL("CREATE TABLE pdu (_id INTEGER PRIMARY KEY, thread_id INTEGER, sub_id INTEGER, msg_box INTEGER, date INTEGER, tr_id TEXT, sub TEXT, m_type INTEGER, exp INTEGER$flags)")
    db.execSQL("CREATE TABLE addr (_id INTEGER PRIMARY KEY, msg_id INTEGER, address TEXT, type INTEGER)")
    db.execSQL("CREATE TABLE part (_id INTEGER PRIMARY KEY, mid INTEGER, seq INTEGER, ct TEXT, name TEXT, fn TEXT, chset INTEGER, text TEXT)")
}

/** Inserts a send-request message with one sender address and one text part. Flags are written only when given. */
fun SQLiteDatabase.insertMessage(id: Long, restricted: Boolean? = null, textOnly: Boolean? = null) {
    insertOrThrow("pdu", null, ContentValues().apply {
        put("_id", id); put("thread_id", 1L); put("sub_id", -1L); put("msg_box", 1L); put("date", 1L); put("m_type", 128L)
        restricted?.let { put("restricted", if (it) 1 else 0) }
        textOnly?.let { put("text_only", if (it) 1 else 0) }
    })
    insertOrThrow("addr", null, ContentValues().apply { put("msg_id", id); put("address", "+15550100"); put("type", 137) })
    insertOrThrow("part", null, ContentValues().apply { put("mid", id); put("seq", 0); put("ct", "text/plain"); put("chset", 106); put("text", "body $id") })
}
