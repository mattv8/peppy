package dev.peppy.mobile.messagingapp

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
class SystemBlocklistTest {
    private lateinit var provider: FakeBlockedNumberProvider
    private lateinit var context: Context

    @Before fun setUp() {
        provider = Robolectric.buildContentProvider(FakeBlockedNumberProvider::class.java).create().get()
        ShadowContentResolver.registerProviderInternal("com.android.blockednumber", provider)
        context = ApplicationProvider.getApplicationContext()
    }

    @Test fun readsNormalizedBoundedEntries() {
        provider.rows += arrayOf(" +12025550123 ", " +12025550123 ")
        provider.rows += arrayOf("x".repeat(65), null)
        provider.rows += arrayOf("555", "y".repeat(65))

        assertEquals(
            BlocklistRead.Entries(
                listOf(BlocklistEntry("+12025550123", "+12025550123"), BlocklistEntry("555", null)),
                truncated = false,
            ),
            SystemBlocklist(context, { true }).read(),
        )
    }

    @Test fun reportsTruncationOnlyWhenCapIsExceeded() {
        repeat(5_000) { provider.rows += arrayOf("+$it", null) }
        val exact = SystemBlocklist(context, { true }).read() as BlocklistRead.Entries
        assertEquals(5_000, exact.entries.size)
        assertFalse(exact.truncated)

        provider.rows += arrayOf("+5000", null)
        val capped = SystemBlocklist(context, { true }).read() as BlocklistRead.Entries
        assertEquals(5_000, capped.entries.size)
        assertTrue(capped.truncated)
    }

    @Test fun doesNotQueryWithoutAccess() {

        provider.queryCount = 0
        assertSame(BlocklistRead.NotAllowed, SystemBlocklist(context, { false }).read())
        assertEquals(0, provider.queryCount)
    }

    @Test fun mapsSecurityExceptionToNotAllowed() {
        provider.deny = true
        assertSame(BlocklistRead.NotAllowed, SystemBlocklist(context, { true }).read())
    }

    @Test fun reconcilesUnblockBeforeReportingSuccess() {
        var blocked = true
        val blocklist = SystemBlocklist(
            context,
            { true },
            { blocked },
            { blocked = false },
        )

        assertEquals(UnblockOutcome.Unblocked, blocklist.unblock("+12025550123"))
        assertEquals(UnblockOutcome.NotBlocked, SystemBlocklist(context, { true }, { false }).unblock("+12025550123"))
        assertEquals(UnblockOutcome.NotAllowed, SystemBlocklist(context, { false }).unblock("+12025550123"))
    }

    @Test fun reportsFailedWhenUnblockDoesNotReconcile() {
        val blocklist = SystemBlocklist(context, { true }, { true }, {})
        assertEquals(UnblockOutcome.Failed, blocklist.unblock("+12025550123"))
    }

    @Test fun leavesInterruptedReconciliationForRetryWhenBlockedStatusIsUnavailable() {
        assertEquals(
            null,
            SystemBlocklist(context, { true }, { throw SecurityException("denied") }).isBlocked("+12025550123"),
        )
        assertEquals(
            null,
            SystemBlocklist(context, { true }, { throw IllegalStateException("unavailable") }).isBlocked("+12025550123"),
        )
    }

    class FakeBlockedNumberProvider : ContentProvider() {
        val rows = mutableListOf<Array<String?>>()
        var deny = false
        var queryCount = 0

        override fun onCreate() = true

        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor {
            queryCount++
            if (deny) throw SecurityException("denied")
            return MatrixCursor(arrayOf("original_number", "e164_number")).apply { rows.forEach(::addRow) }
        }

        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0
    }
}
