package dev.peppy.mobile.mms

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteException
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver


@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsProviderTest {
    private lateinit var provider: FakeMmsProvider
    private lateinit var reader: MmsProviderReader

    @Before fun setUp() {
        provider = Robolectric.buildContentProvider(FakeMmsProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("mms", provider)
        reader = MmsProviderReader(ApplicationProvider.getApplicationContext(), MmsOptionalColumns())
    }

    @Test fun preservesAddressGroupsAndInvalidSubscriptionIsNull() {
        provider.messages += message(1, 128, sub = -1)
        provider.addresses[1] = listOf("from" to 137, "to-1" to 151, "to-2" to 151, "cc" to 130, "bcc" to 129)
        provider.parts[1] = listOf(part(10))
        val record = reader.read(1)!!
        assertEquals(listOf("from"), record.sender); assertEquals(listOf("to-1", "to-2"), record.toRecipients)
        assertEquals(listOf("cc"), record.ccRecipients); assertEquals(listOf("bcc"), record.bccRecipients)
        assertNull(record.subscriptionId)
    }

    @Test fun keepsPendingAndUnsupportedRowsVisibleWithoutPassingOutboxCheckpoint() {
        provider.messages += message(5, 128, box = 4)
        provider.messages += message(6, 134)
        provider.parts[5] = listOf(part(50)); provider.parts[6] = listOf(part(60))
        val page = reader.page(0, 50)
        assertEquals(6L, page.scannedThroughId)
        assertEquals("unsupported_type", page.records.single { it.id == 6L }.error)
        assertTrue(page.records.any { it.id == 5L })
    }

    @Test fun scansDescendingBetweenFrozenBounds() {
        (1..8).forEach { id -> provider.messages += message(id.toLong(), 128); provider.parts[id.toLong()] = listOf(part(id.toLong())) }

        val page = reader.descendingPage(cursor = null, upperBound = 7, floor = 3, limit = 50)

        assertEquals(listOf(7L, 6L, 5L, 4L), page.records.map { it.id })
        assertEquals(4L, page.scannedThroughId)
    }

    @Test fun nullProviderCursorIsAFailureRatherThanAnExhaustedPage() {
        provider.returnNull = true

        assertThrows(MmsProviderException::class.java) { reader.descendingPage(null, 1, null, 50) }
    }

    @Test fun nullMessageAddressAndPartsCursorsAreProviderFailuresNotEmptyRows() {
        provider.messages += message(1, 128)
        provider.nullMessageCursor = true
        assertThrows(MmsProviderException::class.java) { reader.read(1) }

        provider.nullMessageCursor = false
        provider.nullAddressCursor = true
        assertThrows(MmsProviderException::class.java) { reader.read(1) }

        provider.nullAddressCursor = false
        provider.nullPartsCursor = true
        assertThrows(MmsProviderException::class.java) { reader.read(1) }
    }

    @Test fun pendingUnsupportedZeroAndTooManyPartsAreRecordsAndPageContinues() {
        provider.messages += message(1, 130)
        provider.messages += message(2, 134)
        provider.messages += message(3, 128)
        provider.messages += message(4, 128)
        provider.parts[4] = (1..11).map { part(it.toLong()) }
        provider.messages += message(5, 128); provider.parts[5] = listOf(part(50))
        val page = reader.page(0, 50)
        assertEquals("provider_content_pending", page.records.single { it.id == 1L }.error)
        assertEquals("unsupported_type", page.records.single { it.id == 2L }.error)
        assertEquals("provider_content_pending", page.records.single { it.id == 3L }.error)
        assertEquals("too_many_parts", page.records.single { it.id == 4L }.error)
        assertTrue(page.records.single { it.id == 5L }.ready)
    }

    @Test fun preservesOtherwiseCompleteNullDateAndOnlyExpiresPastNotifications() {
        provider.messages += message(1, 132, dateSeconds = null)
        provider.messages += message(2, 130, expirySeconds = 1)
        provider.messages += message(3, 130, expirySeconds = System.currentTimeMillis() / 1_000L + 3_600L)
        provider.messages += message(4, 130, expirySeconds = Long.MAX_VALUE)
        provider.messages += message(5, 130, expirySeconds = 0)
        provider.messages += message(6, 130, expirySeconds = -1)
        provider.parts[1] = listOf(part(10))

        val nullDate = reader.read(1)!!
        assertTrue(nullDate.ready)
        assertNull(nullDate.dateMs)
        assertEquals("notification_expired", reader.read(2)!!.error)
        assertEquals("provider_content_pending", reader.read(3)!!.error)
        assertEquals("provider_content_pending", reader.read(4)!!.error)
        assertEquals("provider_content_pending", reader.read(5)!!.error)
        assertEquals("provider_content_pending", reader.read(6)!!.error)
    }

    @Test fun capsRowsWhenProviderIgnoresLimitAndFlagsOversizedFields() {
        repeat(60) { n -> provider.messages += message(n.toLong() + 1, 128) ; provider.parts[n.toLong() + 1] = listOf(part(n.toLong() + 1)) }
        assertEquals(50, reader.page(0, 50).records.size)
        provider.messages.clear(); provider.parts.clear(); provider.addresses.clear()
        provider.messages += message(1, 128, subject = "x".repeat(MmsProviderReader.MAX_STRING + 1)); provider.parts[1] = listOf(part(1))
        assertEquals("metadata_overflow", reader.read(1)!!.error)
        provider.messages.clear(); provider.messages += message(2, 128); provider.parts[2] = listOf(part(2)); provider.addresses[2] = listOf("x".repeat(MmsProviderReader.MAX_STRING + 1) to 151)
        assertEquals("address_overflow", reader.read(2)!!.error)
    }

    @Test fun utf8BoundsRejectMultibyteValuesWhoseUtf16LengthFitsTheLimit() {
        val subject = "é".repeat(MmsProviderReader.MAX_STRING / 2 + 1)
        provider.messages += message(1, 128, subject = subject)
        provider.parts[1] = listOf(part(1))

        assertTrue(subject.length <= MmsProviderReader.MAX_STRING)
        assertTrue(subject.toByteArray(Charsets.UTF_8).size > MmsProviderReader.MAX_STRING)
        assertEquals("metadata_overflow", reader.read(1)!!.error)
    }

    @Test fun overlongAddressDoesNotStopCollectionOfLaterRecipients() {
        provider.messages += message(1, 128)
        provider.parts[1] = listOf(part(1))
        provider.addresses[1] = listOf("x".repeat(MmsProviderReader.MAX_STRING + 1) to 151, "later-recipient" to 151)

        val record = reader.read(1)!!

        assertEquals("address_overflow", record.error)
        assertEquals(listOf("later-recipient"), record.toRecipients)
    }

    @Test fun utf8BoundsRetainMalformedSurrogateReplacementAtTheBoundary() {
        val allowed = "\uD800".repeat(MmsProviderReader.MAX_STRING)
        val oversized = allowed + "\uD800"
        provider.messages += message(1, 128, subject = allowed)
        provider.messages += message(2, 128, subject = oversized)
        provider.parts[1] = listOf(part(1))
        provider.parts[2] = listOf(part(2))

        assertEquals(MmsProviderReader.MAX_STRING, allowed.toByteArray(Charsets.UTF_8).size)
        assertNull(reader.read(1)!!.error)
        assertEquals("metadata_overflow", reader.read(2)!!.error)
    }

    @Test fun decodesKnownCharsetsAndRejectsOversizedInlineText() {
        val utf16 = MmsProviderPart(1, null, "text/plain", null, 1015, "hello", Uri.parse("content://mms/part/1"))
        assertEquals("hello", reader.readPartText(utf16))
        val oversized = utf16.copy(text = "x".repeat(MmsProviderReader.MAX_TEXT + 1))
        assertThrows(MmsProviderException::class.java) { reader.readPartText(oversized) }
        assertThrows(MmsProviderException::class.java) { reader.readPartText(utf16.copy(charset = 9999)) }
    }

    @Test fun providerWithoutFlagColumnsStillReadsAndFlagsDefaultToFalse() {
        provider.optionalColumns = false
        provider.messages += message(1, 128)
        provider.parts[1] = listOf(part(10))

        val record = reader.read(1)!!

        assertNull(record.error)
        assertFalse(record.restricted)
        assertFalse(record.textOnly)
    }

    @Test fun providerWithFlagColumnsReadsRestrictedAndTextOnly() {
        provider.messages += message(1, 128, restricted = true, textOnly = true)
        provider.messages += message(2, 128)
        provider.parts[1] = listOf(part(10)); provider.parts[2] = listOf(part(20))

        val flagged = reader.read(1)!!
        val plain = reader.read(2)!!

        assertTrue(flagged.restricted)
        assertTrue(flagged.textOnly)
        assertFalse(plain.restricted)
        assertFalse(plain.textOnly)
    }

    @Test fun optionalColumnsAreProbedOnceWhenProviderHasThem() {
        (1..3).forEach { id -> provider.messages += message(id.toLong(), 128); provider.parts[id.toLong()] = listOf(part(id.toLong())) }

        repeat(3) { reader.read(1); reader.read(2); reader.read(3) }

        assertEquals(1, provider.probes)
    }

    @Test fun optionalColumnsAreProbedOnceWhenProviderLacksThem() {
        provider.optionalColumns = false
        (1..3).forEach { id -> provider.messages += message(id.toLong(), 128); provider.parts[id.toLong()] = listOf(part(id.toLong())) }

        repeat(3) { reader.read(1); reader.read(2); reader.read(3) }

        assertEquals(1, provider.probes)
    }

    @Test fun legacySqliteSchemaWithoutFlagColumnsFallsBackAndReadsRowsWithFlagsFalse() {
        SQLiteDatabase.create(null).use { db ->
            createMmsTables(db, flagColumns = false)
            db.insertMessage(1); db.insertMessage(2)
            val sqlite = registerSqliteProvider(db)
            assertThrows(SQLiteException::class.java) { db.query("pdu", arrayOf("_id", "restricted"), null, null, null, null, null) }
            val legacyReader = MmsProviderReader(ApplicationProvider.getApplicationContext(), MmsOptionalColumns())

            val first = legacyReader.read(1)!!
            val second = legacyReader.read(2)!!
            val page = legacyReader.page(0, 50)

            assertNull(first.error); assertNull(second.error)
            assertEquals(listOf(1L, 2L), page.records.map { it.id })
            assertTrue(page.records.all { it.ready && !it.restricted && !it.textOnly })
            assertEquals(1, sqlite.optionalProbes)
        }
    }

    @Test fun modernSqliteSchemaReadsRestrictedAndTextOnlyFlags() {
        SQLiteDatabase.create(null).use { db ->
            createMmsTables(db, flagColumns = true)
            db.insertMessage(1, restricted = true, textOnly = true)
            db.insertMessage(2, restricted = false, textOnly = false)
            val sqlite = registerSqliteProvider(db)
            val modernReader = MmsProviderReader(ApplicationProvider.getApplicationContext(), MmsOptionalColumns())

            val flagged = modernReader.read(1)!!
            val plain = modernReader.read(2)!!

            assertTrue(flagged.restricted); assertTrue(flagged.textOnly)
            assertFalse(plain.restricted); assertFalse(plain.textOnly)
            assertEquals(1, sqlite.optionalProbes)
        }
    }

    private fun registerSqliteProvider(db: SQLiteDatabase): SqliteMmsProvider {
        val sqlite = Robolectric.buildContentProvider(SqliteMmsProvider::class.java).create().get()
        sqlite.database = db
        ShadowContentResolver.registerProviderInternal("mms", sqlite)
        return sqlite
    }

    private fun message(
        id: Long,
        type: Int,
        box: Int = 1,
        sub: Int = 1,
        subject: String? = null,
        dateSeconds: Long? = 1,
        expirySeconds: Long? = null,
        restricted: Boolean = false,
        textOnly: Boolean = false,
    ) = Msg(id, type, box, sub, subject, dateSeconds, expirySeconds, restricted, textOnly)
    private fun part(id: Long) = Part(id)

    data class Msg(
        val id: Long,
        val type: Int,
        val box: Int,
        val sub: Int,
        val subject: String?,
        val dateSeconds: Long?,
        val expirySeconds: Long?,
        val restricted: Boolean = false,
        val textOnly: Boolean = false,
    )
    data class Part(val id: Long)
    /** Rejects unknown projection columns like real providers do when a column is absent from the schema. */
    class FakeMmsProvider : ContentProvider() {
        val messages = mutableListOf<Msg>(); val parts = mutableMapOf<Long, List<Part>>(); val addresses = mutableMapOf<Long, List<Pair<String, Int>>>()
        var returnNull = false
        var nullMessageCursor = false
        var nullAddressCursor = false
        var nullPartsCursor = false
        /** Whether the schema has the optional `restricted` and `text_only` columns (absent on Android 14 AOSP). */
        var optionalColumns = true
        /** Queries of the base URI whose projection names an optional column; the production probe. */
        var probes = 0
        override fun onCreate() = true
        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
            if (returnNull) return null
            val segments = uri.pathSegments
            val isAddress = segments.size == 2 && segments[1] == "addr"
            val isPart = segments.firstOrNull() == "part"
            val isBase = segments.isEmpty()
            val nullCursor = when { isBase -> false; isAddress -> nullAddressCursor; isPart -> nullPartsCursor; else -> nullMessageCursor }
            if (nullCursor) return null
            val known = when {
                isBase -> setOf("_id") + if (optionalColumns) OPTIONAL_COLUMNS else emptySet()
                isAddress -> setOf("address", "type")
                isPart -> setOf("_id", "seq", "ct", "name", "fn", "chset", "text")
                else -> setOf("_id", "thread_id", "sub_id", "msg_box", "date", "tr_id", "sub", "m_type", "exp") + if (optionalColumns) OPTIONAL_COLUMNS else emptySet()
            }
            val requested = projection?.toList().orEmpty()
            if (isBase && requested.any { it in OPTIONAL_COLUMNS }) probes++
            requested.firstOrNull { it !in known }?.let { throw IllegalArgumentException("Unknown column $it") }
            val rows: List<Map<String, Any?>> = when {
                isBase -> messages.filter { message ->
                    when (selection) {
                        "_id > ?" -> message.id > selectionArgs!!.single().toLong()
                        "_id <= ? AND _id > ?" -> message.id <= selectionArgs!![0].toLong() && message.id > selectionArgs[1].toLong()
                        "_id < ? AND _id > ?" -> message.id < selectionArgs!![0].toLong() && message.id > selectionArgs[1].toLong()
                        else -> true
                    }
                }.let { rows -> if (sortOrder?.contains("DESC") == true) rows.sortedByDescending { it.id } else rows.sortedBy { it.id } }
                    .map { mapOf("_id" to it.id) }
                isAddress -> addresses[segments[0].toLong()].orEmpty().map { mapOf("address" to it.first, "type" to it.second) }
                isPart -> parts[selectionArgs!!.single().toLong()].orEmpty().mapIndexed { i, p -> mapOf("_id" to p.id, "seq" to i, "ct" to "text/plain", "chset" to 106) }
                else -> messages.find { it.id == segments.single().toLong() }?.let { listOf(messageRow(it)) }.orEmpty()
            }
            val columns = requested.toTypedArray()
            return MatrixCursor(columns).also { c -> rows.forEach { row -> c.addRow(columns.map { row[it] }.toTypedArray()) } }
        }
        private fun messageRow(m: Msg): Map<String, Any?> = mapOf(
            "_id" to m.id, "thread_id" to 1L, "sub_id" to m.sub.toLong(), "msg_box" to m.box.toLong(), "date" to m.dateSeconds,
            "tr_id" to null, "sub" to m.subject, "m_type" to m.type.toLong(), "exp" to m.expirySeconds,
            "restricted" to (if (m.restricted) 1L else 0L), "text_only" to (if (m.textOnly) 1L else 0L),
        )
        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0
        companion object { private val OPTIONAL_COLUMNS = setOf("restricted", "text_only") }
    }
}
