package dev.peppy.mobile

import android.content.Context
import android.content.ContentProvider
import android.content.ContentValues
import android.app.Application
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.Robolectric
import org.robolectric.shadows.ShadowContentResolver
import java.io.IOException
import java.util.concurrent.CancellationException
import java.lang.reflect.Proxy
import org.robolectric.Shadows.shadowOf
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeGatewayPolicyDecision

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PhoneReadIngestTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test fun providerUsesReadNotSeenAndRequiresExactSmsIdentity() {
        val provider = Providers.install()
        provider.sms["1"] = sms(read = 0, seen = 1)
        assertEquals(PhoneReadProviderResult.UNREAD, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
        provider.sms["1"] = sms(read = 1, seen = 0)
        assertEquals(PhoneReadProviderResult.READ, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
        provider.sms["1"] = sms(read = 1, thread = 99)
        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
        provider.sms["1"] = sms(read = 1, address = "other")
        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
    }

    @Test fun providerSkipsMalformedRowsAndRetriesProviderFailures() {
        val provider = Providers.install()
        provider.sms["1"] = sms(read = 1)
        provider.columns = arrayOf("_id", "thread_id", "sub_id", "address", "date", "type", "creator")
        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
        provider.columns = null; provider.nullCursor = true
        assertEquals(PhoneReadProviderResult.UNAVAILABLE, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
        provider.nullCursor = false; provider.security = true
        assertEquals(PhoneReadProviderResult.UNAVAILABLE, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
    }

    @Test fun providerPropagatesCancellationAndChecksMmsIdentityAndOverflow() {
        val provider = Providers.install()
        provider.cancel = true
        try { AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()); fail("expected cancellation") } catch (_: CancellationException) {}
        provider.cancel = false
        provider.mms["7"] = mms(date = 9, read = 1, transaction = "transaction")
        val mms = PhoneReadCandidate("mms", PhoneReadTransport.MMS, 1, PhoneReadProviderRow("generation", "android-subscription-4", "7", "8", 9_000, ""), "transaction")
        assertEquals(PhoneReadProviderResult.READ, AndroidPhoneReadIngestProvider(context).isRead(mms))
        provider.mms["7"] = mms(date = Long.MAX_VALUE / 1_000 + 1, read = 1, transaction = "transaction")
        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(mms))
        provider.mms["7"] = mms(date = 9, read = 1, transaction = "wrong", box = 2)
        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(mms))
    }

    @Test fun skipsUnreadRowsThenReachesReadCandidatesBeyondFiveHundredAcrossPasses() {
        val core = FakeCore((1L..700L).map(::candidate))
        val provider = FakeProvider().apply { readOrders += 501L }
        val runner = runner(core, provider)

        assertFalse(runner.run())
        assertEquals(emptyList<String>(), core.applied)
        assertTrue(runner.run())
        assertEquals(listOf("message-501"), core.applied)
    }

    @Test fun missingRowsAreNeverApplied() {
        val core = FakeCore(listOf(candidate(1), candidate(2)))
        val provider = FakeProvider().apply { readOrders += 1L; missingOrders += 1L }

        assertFalse(runner(core, provider).run())
        assertEquals(emptyList<String>(), core.applied)
    }

    @Test fun policyIsRecheckedImmediatelyBeforeApply() {
        val core = FakeCore(listOf(candidate(1)))
        val provider = FakeProvider().apply { readOrders += 1L }
        val policy = FakePolicy(allowed = true, changeAfterFirstCheck = false)
        val runner = PhoneReadIngestRunner(context, core, provider, policy, InMemoryPhoneReadCursorStore(), { "generation" }, { true })

        policy.changeAfterFirstCheck = true
        assertFalse(runner.run())
        assertEquals(emptyList<String>(), core.applied)
    }

    @Test fun onlyReadFlagAuthorizesMmsWithExactStoredIdentity() {
        val row = PhoneReadProviderRow("generation", "android-subscription-4", "7", "8", 9_000L, "")
        val candidate = PhoneReadCandidate("mms-7", PhoneReadTransport.MMS, 1, row, "transaction")
        val core = FakeCore(listOf(candidate))
        val provider = FakeProvider().apply { readOrders += 1L }

        assertTrue(runner(core, provider).run())
        assertEquals(listOf("mms-7"), core.applied)
    }

    @Test fun providerCancellationPropagates() {
        val core = FakeCore(listOf(candidate(1)))
        val provider = FakeProvider().apply { cancelOrders += 1L }

        try {
            runner(core, provider).run()
            fail("expected cancellation")
        } catch (_: CancellationException) {
            // A cancelled Worker must remain cancelled rather than being retried as a provider error.
        }
    }

    @Test fun candidateQueryCheckedExceptionReturnsFalseWithoutSavingCursor() {
        val core = object : PhoneReadIngestCore {
            override fun candidates(transport: PhoneReadTransport, sourceGeneration: String, afterOrder: Long, limit: Int): List<PhoneReadCandidate> =
                throw IOException("candidate query unavailable")

            override fun apply(candidates: List<PhoneReadCandidate>): Int = 0
        }
        val cursors = RecordingPhoneReadCursorStore()

        assertFalse(customRunner(core, cursors = cursors).run())
        assertEquals(emptyList<Triple<PhoneReadTransport, String, Long>>(), cursors.saves)
    }

    @Test fun applyCheckedExceptionKeepsCursorBeforeFailedCandidate() {
        val core = FakeCore(listOf(candidate(1), candidate(2))).apply { applyFailure = IOException("write failed") }
        val provider = FakeProvider().apply { readOrders += setOf(1L, 2L) }
        val cursors = RecordingPhoneReadCursorStore()
        val runner = customRunner(core, provider, cursors = cursors)

        assertFalse(runner.run())
        assertEquals(emptyList<String>(), core.applied)
        assertEquals(listOf(0L), smsCursorSaves(cursors))

        core.applyFailure = null
        assertTrue(runner.run())
        assertEquals(listOf("message-1", "message-2"), core.applied)
    }

    @Test fun invalidEarlyCandidatesAdvanceSoLaterValidReadApplies() {
        val zeroThread = PhoneReadCandidate("message-1", PhoneReadTransport.SMS, 1,
            PhoneReadProviderRow("generation", "android-subscription-4", "1", "0", 9_000L, "fingerprint-1"), null)
        val nonNumericId = PhoneReadCandidate("message-2", PhoneReadTransport.SMS, 2,
            PhoneReadProviderRow("generation", "android-subscription-4", "two", "8", 9_000L, "fingerprint-2"), null)
        val core = FakeCore(listOf(zeroThread, nonNumericId, candidate(3)))
        val provider = FakeProvider().apply { readOrders += setOf(1L, 2L, 3L) }

        assertTrue(customRunner(core, provider).run())
        assertEquals(listOf("message-3"), core.applied)
    }

    @Test fun nullColumnProviderRowIsSkippedAndLaterValidReadApplies() {
        val provider = Providers.install()
        provider.sms["1"] = mapOf(
            "_id" to 1L, "thread_id" to null, "sub_id" to 4L, "address" to "sender", "date" to 9_000L,
            "type" to 1L, "read" to 1L, "seen" to 0L, "creator" to "creator",
        )
        provider.sms["2"] = sms(read = 1, id = 2L)
        val validRecord = SmsProviderRecord(2, 8, 4, "sender", null, 9_000, null, 1, true, null, null, "creator")
        val validCandidate = PhoneReadCandidate("message-2", PhoneReadTransport.SMS, 2,
            PhoneReadProviderRow("generation", "android-subscription-4", "2", "8", 9_000, SmsCapture.sourceRowFingerprint(validRecord)), null)
        val core = FakeCore(listOf(candidate(1), validCandidate))

        assertTrue(customRunner(core, AndroidPhoneReadIngestProvider(context)).run())
        assertEquals(listOf("message-2"), core.applied)
    }

    @Test fun permissionRevokedAfterProviderReadBlocksApply() {
        var permitted = true
        val provider = object : PhoneReadIngestProvider {
            override fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult {
                permitted = false
                return PhoneReadProviderResult.READ
            }
        }
        val core = FakeCore(listOf(candidate(1)))
        val cursors = RecordingPhoneReadCursorStore()

        assertFalse(customRunner(core, provider, cursors = cursors, permitted = { permitted }).run())
        assertEquals(emptyList<String>(), core.applied)
        assertEquals(listOf(0L), smsCursorSaves(cursors))
    }

    @Test fun mmsPolicyDenialStillAppliesSmsRead() {
        val core = FakeCore(listOf(candidate(1), mmsCandidate(2)))
        val provider = FakeProvider().apply { readOrders += setOf(1L, 2L) }
        val policy = object : PhoneReadIngestPolicy {
            override fun allowed(transport: PhoneReadTransport) = transport == PhoneReadTransport.SMS
        }

        assertTrue(customRunner(core, provider, policy = policy).run())
        assertEquals(listOf("message-1"), core.applied)
    }

    @Test fun productionPolicyRequiresEnabledMmsPreferenceAtScanAndApplyButPermitsSms() {
        val application = context.applicationContext as Application
        val readSmsWasGranted = smsReadGranted(context)
        shadowOf(application).grantPermissions(android.Manifest.permission.READ_SMS)
        val preferences = MmsPreferences(context)
        val policy = NativePhoneReadIngestPolicy(context, policyClient(captureMms = true))
        try {
            preferences.enabled = false
            val initialCore = FakeCore(listOf(candidate(1), mmsCandidate(2)))
            val initialProvider = FakeProvider().apply { readOrders += setOf(1L, 2L) }

            assertTrue(customRunner(initialCore, initialProvider, policy).run())
            assertEquals(listOf("message-1"), initialCore.applied)

            preferences.enabled = true
            val toggledCore = FakeCore(listOf(mmsCandidate(2)))
            val toggledProvider = object : PhoneReadIngestProvider {
                override fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult {
                    preferences.enabled = false
                    return PhoneReadProviderResult.READ
                }
            }

            assertFalse(customRunner(toggledCore, toggledProvider, policy).run())
            assertEquals(emptyList<String>(), toggledCore.applied)
        } finally {
            preferences.enabled = false
            shadowOf(application).denyPermissions(android.Manifest.permission.READ_SMS)
            if (readSmsWasGranted) shadowOf(application).grantPermissions(android.Manifest.permission.READ_SMS)
        }
    }

    @Test fun exhaustedNonzeroCursorWrapsToEarlierReadInSameRun() {
        val cursors = InMemoryPhoneReadCursorStore().apply { save(PhoneReadTransport.SMS, "generation", 50L) }
        val core = FakeCore((1L..10L).map(::candidate))
        val provider = FakeProvider().apply { readOrders += 1L }

        assertTrue(customRunner(core, provider, cursors = cursors).run())
        assertEquals(listOf("message-1"), core.applied)
    }

    @Test fun smsAndMmsCursorProgressAreStoredSeparately() {
        val cursors = RecordingPhoneReadCursorStore()
        val core = FakeCore((1L..250L).map(::candidate))

        assertFalse(customRunner(core, cursors = cursors).run())
        assertEquals(listOf(100L, 200L, 0L), smsCursorSaves(cursors))
        assertEquals(setOf(0L), cursors.saves.filter { it.first == PhoneReadTransport.MMS }.map { it.third }.toSet())
    }

    @Test fun mmsWrongTransactionWithCorrectBoxIsMissing() {
        val provider = Providers.install()
        provider.mms["7"] = mms(date = 9, read = 1, transaction = "wrong")

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(mmsCandidate(1)))
    }

    @Test fun mmsWrongBoxWithCorrectTransactionIsMissing() {
        val provider = Providers.install()
        provider.mms["7"] = mms(date = 9, read = 1, transaction = "transaction", box = 2)

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(mmsCandidate(1)))
    }

    @Test fun mmsUnknownSubscriptionDoesNotUseTheSmsFallbackRoute() {
        val provider = Providers.install()
        provider.mms["7"] = mms(date = 9, read = 1, transaction = "transaction").mapValues { it.value as Any? }.toMutableMap().apply {
            this["sub_id"] = null
        }
        val candidate = PhoneReadCandidate(
            "mms-unknown-subscription", PhoneReadTransport.MMS, 1,
            PhoneReadProviderRow("generation", SmsCapture.UNKNOWN_SUBSCRIPTION, "7", "8", 9_000L, ""),
            "transaction",
        )

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(candidate))
    }

    @Test fun smsDateMismatchIsMissing() {
        val provider = Providers.install()
        provider.sms["1"] = sms(read = 1, date = 9_001L)

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
    }

    @Test fun smsSubscriptionMismatchIsMissing() {
        val provider = Providers.install()
        provider.sms["1"] = sms(read = 1, sub = 5L)

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
    }

    @Test fun smsCreatorMismatchIsMissing() {
        val provider = Providers.install()
        provider.sms["1"] = sms(read = 1, creator = "other")

        assertEquals(PhoneReadProviderResult.MISSING, AndroidPhoneReadIngestProvider(context).isRead(providerCandidate()))
    }

    @Test fun startingAtZeroScansShortPageOnceWithoutRedundantWrap() {
        val core = FakeCore(listOf(candidate(1)))
        var providerCalls = 0
        val provider = object : PhoneReadIngestProvider {
            override fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult {
                providerCalls++
                return PhoneReadProviderResult.UNREAD
            }
        }
        val cursors = RecordingPhoneReadCursorStore()

        assertFalse(customRunner(core, provider, cursors = cursors).run())
        assertEquals(1, providerCalls)
        assertEquals(listOf(0L), smsCursorSaves(cursors))
    }

    @Test fun emptyTransportsAtZeroSaveZeroOnce() {
        val cursors = RecordingPhoneReadCursorStore()

        assertFalse(customRunner(FakeCore(emptyList()), cursors = cursors).run())
        assertEquals(listOf(0L), smsCursorSaves(cursors))
        assertEquals(listOf(0L), cursors.saves.filter { it.first == PhoneReadTransport.MMS }.map { it.third })
    }

    private fun smsCursorSaves(cursors: RecordingPhoneReadCursorStore) =
        cursors.saves.filter { it.first == PhoneReadTransport.SMS }.map { it.third }

    private fun mmsCandidate(order: Long) = PhoneReadCandidate(
        "mms-$order", PhoneReadTransport.MMS, order,
        PhoneReadProviderRow("generation", "android-subscription-4", "7", "8", 9_000L, ""),
        "transaction",
    )

    private fun customRunner(
        core: PhoneReadIngestCore,
        provider: PhoneReadIngestProvider = FakeProvider(),
        policy: PhoneReadIngestPolicy = FakePolicy(),
        cursors: PhoneReadCursorStore = InMemoryPhoneReadCursorStore(),
        permitted: () -> Boolean = { true },
    ) = PhoneReadIngestRunner(context, core, provider, policy, cursors, { "generation" }, permitted)

    private fun policyClient(captureMms: Boolean): NativeClientInterface = Proxy.newProxyInstance(
        NativeClientInterface::class.java.classLoader,
        arrayOf(NativeClientInterface::class.java),
    ) { _, method, _ ->
        when (method.name) {
            "gatewayPolicyDecision" -> NativeGatewayPolicyDecision(false, true, captureMms, false, false, false, true)
            "toString" -> "phone-read-policy-client"
            else -> error("unexpected native policy call: ${method.name}")
        }
    } as NativeClientInterface

    private class InMemoryPhoneReadCursorStore : PhoneReadCursorStore {
        private val cursors = mutableMapOf<Pair<PhoneReadTransport, String>, Long>()
        override fun cursor(transport: PhoneReadTransport, generation: String) = cursors[transport to generation] ?: 0L
        override fun save(transport: PhoneReadTransport, generation: String, cursor: Long) {
            cursors[transport to generation] = cursor
        }
    }

    private class RecordingPhoneReadCursorStore : PhoneReadCursorStore {
        private val cursors = mutableMapOf<Pair<PhoneReadTransport, String>, Long>()
        val saves = mutableListOf<Triple<PhoneReadTransport, String, Long>>()
        override fun cursor(transport: PhoneReadTransport, generation: String) = cursors[transport to generation] ?: 0L
        override fun save(transport: PhoneReadTransport, generation: String, cursor: Long) {
            cursors[transport to generation] = cursor
            saves += Triple(transport, generation, cursor)
        }
    }

    private fun runner(core: FakeCore, provider: FakeProvider) =
        PhoneReadIngestRunner(context, core, provider, FakePolicy(), InMemoryPhoneReadCursorStore(), { "generation" }, { true })

    private fun candidate(order: Long) = PhoneReadCandidate(
        "message-$order", PhoneReadTransport.SMS, order,
        PhoneReadProviderRow("generation", "android-subscription-4", order.toString(), "8", 9_000L, "fingerprint-$order"),
        null,
    )

    private fun providerCandidate(): PhoneReadCandidate {
        val row = SmsProviderRecord(1, 8, 4, "sender", null, 9_000, null, 1, true, null, null, "creator")
        return PhoneReadCandidate("message-1", PhoneReadTransport.SMS, 1,
            PhoneReadProviderRow("generation", "android-subscription-4", "1", "8", 9_000, SmsCapture.sourceRowFingerprint(row)), null)
    }

    private class FakeCore(candidates: List<PhoneReadCandidate>) : PhoneReadIngestCore {
        private val pending = candidates.toMutableList()
        val applied = mutableListOf<String>()
        var applyFailure: Exception? = null
        override fun candidates(transport: PhoneReadTransport, sourceGeneration: String, afterOrder: Long, limit: Int) =
            pending.filter { it.transport == transport && it.localOrder > afterOrder }.take(limit)

        override fun apply(candidates: List<PhoneReadCandidate>): Int {
            applyFailure?.let { throw it }
            applied += candidates.map { it.messageId }
            pending.removeAll(candidates.toSet())
            return candidates.size
        }
    }

    private class FakeProvider : PhoneReadIngestProvider {
        val readOrders = mutableSetOf<Long>()
        val missingOrders = mutableSetOf<Long>()
        val cancelOrders = mutableSetOf<Long>()
        override fun isRead(candidate: PhoneReadCandidate): PhoneReadProviderResult = when (candidate.localOrder) {
            in cancelOrders -> throw CancellationException("cancelled")
            in missingOrders -> PhoneReadProviderResult.MISSING
            in readOrders -> PhoneReadProviderResult.READ
            else -> PhoneReadProviderResult.UNREAD
        }
    }

    private fun sms(
        read: Long,
        seen: Long = 0,
        thread: Long = 8,
        address: String = "sender",
        id: Long = 1L,
        date: Long = 9_000L,
        sub: Long = 4L,
        creator: String = "creator",
    ) = mapOf(
        "_id" to id, "thread_id" to thread, "sub_id" to sub, "address" to address, "date" to date,
        "type" to 1L, "read" to read, "seen" to seen, "creator" to creator,
    )
    private fun mms(date: Long, read: Long, transaction: String, box: Long = 1) = mapOf(
        "_id" to 7L, "thread_id" to 8L, "sub_id" to 4L, "msg_box" to box, "date" to date, "read" to read, "tr_id" to transaction,
    )

    private class Providers : ContentProvider() {
        val sms = mutableMapOf<String, Map<String, Any?>>()
        val mms = mutableMapOf<String, Map<String, Any?>>()
        var nullCursor = false; var security = false; var cancel = false; var columns: Array<String>? = null
        override fun onCreate() = true
        override fun query(uri: Uri, projection: Array<out String>?, selection: String?, args: Array<out String>?, sort: String?): Cursor? {
            if (cancel) throw CancellationException("cancel")
            if (security) throw SecurityException("denied")
            if (nullCursor) return null
            val row = (if (uri.authority == "sms") sms else mms)[uri.lastPathSegment] ?: return MatrixCursor(columns ?: projection ?: emptyArray())
            val requested = columns ?: projection?.map { it }.orEmpty().toTypedArray().ifEmpty { row.keys.toTypedArray() }
            return MatrixCursor(requested).apply { addRow(requested.map { row[it] }) }
        }
        override fun getType(uri: Uri): String? = null
        override fun insert(uri: Uri, values: ContentValues?): Uri? = null
        override fun delete(uri: Uri, selection: String?, args: Array<out String>?) = 0
        override fun update(uri: Uri, values: ContentValues?, selection: String?, args: Array<out String>?) = 0
        companion object {
            fun install(): Providers {
                val provider = Robolectric.buildContentProvider(Providers::class.java).create().get()
                ShadowContentResolver.registerProviderInternal("sms", provider)
                ShadowContentResolver.registerProviderInternal("mms", provider)
                return provider
            }
        }
    }

    private class FakePolicy(
        var allowed: Boolean = true,
        var changeAfterFirstCheck: Boolean = false,
    ) : PhoneReadIngestPolicy {
        private var checks = 0
        override fun allowed(transport: PhoneReadTransport): Boolean = allowed && (++checks == 1 || !changeAfterFirstCheck)
    }
}
