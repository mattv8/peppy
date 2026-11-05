package dev.peppy.mobile.messagingapp

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
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
class ProviderArchiveReaderTest {
    private lateinit var provider: FakeArchiveProvider
    private lateinit var reader: ProviderArchiveReader

    @Before fun setUp() {
        provider = Robolectric.buildContentProvider(FakeArchiveProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("mms-sms", provider)
        reader = ProviderArchiveReader(ApplicationProvider.getApplicationContext<android.content.Context>().contentResolver)
    }

    @Test fun readsArchivedThreadIdsWithTheExpectedProviderQuery() {
        provider.rows += 8L
        provider.rows += 3L

        val result = reader.archivedThreadIds()

        assertEquals(ProviderArchiveRead.Available(linkedSetOf(8L, 3L), false), result)
        assertEquals("archived=1", provider.selection)
        assertTrue(provider.lastUri.toString().contains("simple=true"))
    }

    @Test fun capsArchiveRowsAndReportsTruncation() {
        repeat(10_001) { provider.rows += it.toLong() }

        val result = reader.archivedThreadIds() as ProviderArchiveRead.Available

        assertEquals(10_000, result.threadIds.size)
        assertTrue(result.truncated)
    }

    @Test fun mapsPermissionAndNullCursorToUnavailable() {
        provider.deny = true
        assertSame(ProviderArchiveRead.Unavailable, reader.archivedThreadIds())

        provider.deny = false
        provider.returnNull = true
        assertSame(ProviderArchiveRead.Unavailable, reader.archivedThreadIds())
    }

    @Test fun missingIdColumnIsUnavailableNotEmpty() {
        provider.rows += 8L
        provider.idColumnName = "thread"

        assertSame(ProviderArchiveRead.Unavailable, reader.archivedThreadIds())
    }

    class FakeArchiveProvider : ContentProvider() {
        val rows = mutableListOf<Long>()
        var deny = false
        var returnNull = false
        var idColumnName = "_id"
        var selection: String? = null
        lateinit var lastUri: Uri

        override fun onCreate() = true

        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
            if (deny) throw SecurityException("denied")
            if (returnNull) return null
            lastUri = uri
            this.selection = selection
            return MatrixCursor(arrayOf(idColumnName)).apply { rows.forEach { addRow(arrayOf(it)) } }
        }

        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0
    }
}
