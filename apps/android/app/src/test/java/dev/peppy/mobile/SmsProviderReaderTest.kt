package dev.peppy.mobile

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
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
class SmsProviderReaderTest {
    private lateinit var provider: FakeSmsProvider
    private lateinit var reader: SmsProviderReader

    @Before fun setUp() {
        provider = Robolectric.buildContentProvider(FakeSmsProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("sms", provider)
        reader = SmsProviderReader(ApplicationProvider.getApplicationContext())
    }

    @Test fun mapsNullableColumnsAndPreservesMillisecondDates() {
        provider.rows += arrayOf(1L, null, -1L, null, null, 1_700_000_000_123L, 1_700_000_000_456L, 1L, null, 1L, null, "messaging")
        val record = reader.read(1)!!
        assertNull(record.threadId)
        assertNull(record.subscriptionId)
        assertNull(record.address)
        assertNull(record.body)
        assertEquals(1_700_000_000_123L, record.dateMs)
        assertEquals(1_700_000_000_456L, record.dateSentMs)
        assertEquals("messaging", record.creator)
        assertNull(record.read)
        assertEquals(true, record.seen)
    }

    @Test fun zeroProviderDatesAreUnknownNotEpoch() {
        provider.rows += arrayOf(2L, 1L, 1L, "+15551234", "sent", 1_790_875_448_413L, 0L, 2L, 1L, 1L, -1L, "dev.peppy.mobile")
        provider.rows += arrayOf(3L, 1L, 1L, "+15551234", "undated", 0L, 0L, 2L, 1L, 1L, -1L, null)
        assertEquals(1_790_875_448_413L, reader.read(2)!!.dateMs)
        assertNull(reader.read(2)!!.dateSentMs)
        assertNull(reader.read(3)!!.dateMs)
    }

    @Test fun missingRowIsAbsentNotAProviderFailure() {
        provider.rows += row(1)
        assertNull(reader.read(2))
    }

    @Test fun scansAscendingAndCapsToFiftyRows() {
        repeat(60) { id -> provider.rows.add(row((60 - id).toLong())) }
        val page = reader.page(0, 500)
        assertEquals(50, page.records.size)
        assertEquals((1L..50L).toList(), page.records.map { it.id })
        assertEquals(50L, page.scannedThroughId)
        assertEquals(60L, reader.highWater())
    }

    @Test fun scansDescendingBetweenFrozenBounds() {
        repeat(8) { id -> provider.rows += row((id + 1).toLong()) }

        val page = reader.descendingPage(cursor = null, upperBound = 7, floor = 3, limit = 50)

        assertEquals(listOf(7L, 6L, 5L, 4L), page.records.map { it.id })
        assertEquals(4L, page.scannedThroughId)
    }

    @Test fun permissionFailureIsNotAnEmptyPage() {
        provider.deny = true
        assertThrows(SmsProviderException::class.java) { reader.page(0, 1) }
    }

    @Test fun nullProviderCursorIsNotAnEmptyPage() {
        provider.returnNull = true
        assertThrows(SmsProviderException::class.java) { reader.page(0, 1) }
    }

    @Test fun diagnosticPassAggregatesQueryTimeWithoutIncludingWorkBetweenQueries() {
        provider.rows += row(1)
        var now = 100L
        provider.onQuery = { now += 7 }
        val events = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { now }, { 0L }, events::add) {
            reader.beginDiagnosticPass()
            assertEquals(1L, reader.read(1)?.id)
            now += 500 // Core/crypto work between queries is not provider-query time.
            provider.deny = true
            assertThrows(SmsProviderException::class.java) { reader.read(2) }
            reader.finishDiagnosticPass()
        }

        val summaries = events.filter { it.contains("event=${SyncTraceEvent.PROVIDER_QUERY.code}") }
        assertEquals(1, summaries.size)
        val summary = summaries.single()
        assertTrue(summary.contains("${SyncTraceField.COUNT.code}=2"))
        assertTrue(summary.contains("${SyncTraceField.ROWS.code}=1"))
        assertTrue(summary.contains("${SyncTraceField.PROVIDER_ERROR.code}=1"))
        assertTrue(summary.contains("${SyncTraceField.DURATION_MS.code}=14"))
    }

    private fun row(id: Long): Array<Any?> = arrayOf(id, 1L, 1L, "+15551234", "body", id * 1000, id * 1000 + 1, 1L, 1L, 1L, 0L, null)

    class FakeSmsProvider : ContentProvider() {
        val rows = mutableListOf<Array<Any?>>()
        var deny = false
        var returnNull = false
        var onQuery: (() -> Unit)? = null
        override fun onCreate() = true
        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
            onQuery?.invoke()
            if (deny) throw SecurityException("denied")
            if (returnNull) return null
            val cursor = MatrixCursor(COLUMNS)
            val requested = uri.lastPathSegment?.toLongOrNull()
            val filtered = rows.filter { row ->
                if (requested != null) return@filter row[0] == requested
                val id = row[0] as Long
                when (selection) {
                    "_id > ?" -> id > selectionArgs!!.single().toLong()
                    "_id <= ? AND _id > ?" -> id <= selectionArgs!![0].toLong() && id > selectionArgs[1].toLong()
                    "_id < ? AND _id > ?" -> id < selectionArgs!![0].toLong() && id > selectionArgs[1].toLong()
                    else -> true
                }
            }
            (if (sortOrder?.contains("DESC") == true) filtered.sortedByDescending { it[0] as Long } else filtered.sortedBy { it[0] as Long })
                .forEach(cursor::addRow)
            return cursor
        }
        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0
        private companion object { val COLUMNS = arrayOf("_id", "thread_id", "sub_id", "address", "body", "date", "date_sent", "type", "read", "seen", "status", "creator") }
    }
}
