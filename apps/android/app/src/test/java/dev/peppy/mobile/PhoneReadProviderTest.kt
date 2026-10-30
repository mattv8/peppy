package dev.peppy.mobile

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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
class PhoneReadProviderTest {
    private lateinit var sms: FakeSmsProvider
    private lateinit var mms: FakeMmsProvider
    private lateinit var provider: AndroidPhoneReadProvider

    @Before fun setUp() {
        sms = Robolectric.buildContentProvider(FakeSmsProvider::class.java).create().get()
        mms = Robolectric.buildContentProvider(FakeMmsProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("sms", sms)
        ShadowContentResolver.registerProviderInternal("mms", mms)
        provider = AndroidPhoneReadProvider(ApplicationProvider.getApplicationContext())
    }

    @Test fun nullCursorMakesProviderUnavailable() {
        sms.returnNull = true
        assertEquals(ProviderRowResult.Unavailable, provider.row("1"))
        assertNull(provider.thread(9))
    }

    @Test fun permissionFailureMakesProviderUnavailable() {
        sms.deny = true
        assertEquals(ProviderRowResult.Unavailable, provider.row("1"))
        assertNull(provider.thread(9))
    }

    @Test fun allUnreadRowsAreReadBeyondFiveHundredHistoricalRows() {
        sms.rows += row(1L, 9, 1_000L, read = false)
        repeat(501) { sms.rows += row(it + 2L, 9, 1_001L + it, read = true) }
        val thread = requireNotNull(provider.thread(9))
        assertEquals(setOf(1_000L), thread.thread.unreadDates)
        assertEquals(502, thread.thread.rowDates.size)
    }

    @Test fun addressAndCreatorUseSmsCaptureBounds() {
        val oversized = "x".repeat(SmsProviderReader.MAX_STRING + 1)
        sms.rows += arrayOf(1L, 9L, 1L, oversized, 1_000L, 1L, 0L, oversized)
        val record = (provider.row("1") as ProviderRowResult.Found).row
        assertNull(record.address)
        assertNull(record.creator)
    }

    @Test fun collisionRequiresOneOtherThreadToContainEntireCandidateSet() {
        sms.rows += row(1, 9, 100, false)
        sms.rows += row(2, 9, 200, false)
        sms.rows += row(3, 10, 100, true)
        sms.rows += row(4, 11, 200, true)
        assertFalse(requireNotNull(provider.collidesWithAnotherThread(9, setOf(100, 200))))
        sms.rows += row(5, 10, 200, true)
        assertTrue(requireNotNull(provider.collidesWithAnotherThread(9, setOf(100, 200))))
    }

    @Test fun nullAndPermissionFailureBlockMmsCheck() {
        mms.returnNull = true
        assertNull(provider.hasUnreadMms(9))
        mms.returnNull = false; mms.deny = true
        assertNull(provider.hasUnreadMms(9))
    }

    private fun row(id: Long, thread: Long, date: Long, read: Boolean) =
        arrayOf<Any?>(id, thread, 1L, "sender", date, 1L, if (read) 1L else 0L, "creator")

    class FakeSmsProvider : ContentProvider() {
        val rows = mutableListOf<Array<Any?>>()
        var returnNull = false
        var deny = false
        override fun onCreate() = true
        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, args: Array<out String>?, sort: String?): Cursor? {
            if (deny) throw SecurityException("denied")
            if (returnNull) return null
            val requested = uri.lastPathSegment?.toLongOrNull()
            val filtered = rows.filter { row ->
                when {
                    requested != null -> row[0] == requested
                    selection == "thread_id=? AND type=? AND read=0" -> row[1] == args!![0].toLong() && row[5] == 1L && row[6] == 0L
                    selection == "thread_id=?" -> row[1] == args!!.single().toLong()
                    selection?.startsWith("thread_id!=? AND date IN") == true -> row[1] != args!![0].toLong() && row[4] in args.drop(1).map(String::toLong).toSet()
                    else -> true
                }
            }
            return MatrixCursor(projection ?: COLUMNS).also { cursor ->
                filtered.forEach { row ->
                    cursor.addRow((projection ?: COLUMNS).map { column ->
                        COLUMNS.indexOf(column).takeIf { it >= 0 }?.let(row::get)
                    })
                }
            }
        }
        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, args: Array<out String>?) = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, args: Array<out String>?) = 0
        companion object { val COLUMNS = arrayOf("_id", "thread_id", "sub_id", "address", "date", "type", "read", "creator") }
    }

    class FakeMmsProvider : ContentProvider() {
        var returnNull = false
        var deny = false
        override fun onCreate() = true
        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, args: Array<out String>?, sort: String?): Cursor? {
            if (deny) throw SecurityException("denied")
            return if (returnNull) null else MatrixCursor(projection ?: arrayOf("_id"))
        }
        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, args: Array<out String>?) = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, args: Array<out String>?) = 0
    }
}
