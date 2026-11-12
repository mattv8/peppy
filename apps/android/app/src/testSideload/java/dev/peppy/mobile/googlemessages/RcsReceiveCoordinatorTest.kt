package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.gateway.GatewayTestBase
import java.util.Collections
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeRcsContextExpectation
import uniffi.peppy_mobile_bindings.NativeRcsIngestOutcome
import uniffi.peppy_mobile_bindings.NativeRcsObservationProvenance
import uniffi.peppy_mobile_bindings.NativeRcsReceiveContext
import uniffi.peppy_mobile_bindings.NativeRcsReceiveOwner
import uniffi.peppy_mobile_bindings.NativeRcsResumeHint

private const val BASE = 1_700_000_000_000L
private const val HOUR = 3_600_000L

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsReceiveCoordinatorTest {
    @Test fun `disabled gate performs no receive or gap effects`() {
        val core = FakeCore(listOf(conv("core-a", "google-a"))).apply { failOnUse = true }
        val coordinator = coordinator(core, enabled = false)
        coordinator.receive(liveRecord("google-a"))
        coordinator.resume(FakeGoogle(emptyMap()))
        assertEquals(0, core.contextsCalls.get())
    }

    @Test fun `resume contains a failed context enumeration`() {
        val core = FakeCore(emptyList()).apply { contextsFailure = IllegalStateException("synthetic") }
        val google = FakeGoogle(emptyMap())

        coordinator(core).resume(google)

        assertEquals(1, core.contextsCalls.get())
        assertTrue(google.requests.isEmpty())
    }

    @Test fun `live events for one conversation are serialized while different conversations overlap`() {
        val activeByConversation = ConcurrentHashMap<String, AtomicInteger>()
        val maxByConversation = ConcurrentHashMap<String, AtomicInteger>()
        val bothConversationsEntered = CountDownLatch(2)
        val release = CountDownLatch(1)
        val core = FakeCore(listOf(conv("core-a", "google-a"), conv("core-b", "google-b"))).apply {
            onIngest = { context, _, _ ->
                val active = activeByConversation.getOrPut(context.conversationId) { AtomicInteger() }.incrementAndGet()
                maxByConversation.getOrPut(context.conversationId) { AtomicInteger() }.updateAndGet { maxOf(it, active) }
                bothConversationsEntered.countDown()
                assertTrue(release.await(5, TimeUnit.SECONDS))
                activeByConversation.getValue(context.conversationId).decrementAndGet()
                accepted()
            }
        }
        val coordinator = coordinator(core)
        val firstA = thread { coordinator.receive(liveRecord("google-a")) }
        assertTrue(waitUntilEntered(activeByConversation, "core-a"))
        val secondA = thread { coordinator.receive(liveRecord("google-a")) }
        val firstB = thread { coordinator.receive(liveRecord("google-b")) }
        assertTrue("different conversations should overlap", bothConversationsEntered.await(5, TimeUnit.SECONDS))
        assertEquals(1, maxByConversation.getValue("core-a").get())
        release.countDown()
        listOf(firstA, secondA, firstB).forEach { it.join(5_000); assertFalse(it.isAlive) }
        assertEquals(1, maxByConversation.getValue("core-a").get())
        assertEquals(1, maxByConversation.getValue("core-b").get())
    }

    @Test fun `first claim retains the trigger message newer than the proof and stops before older history`() {
        val trigger = BASE + 5_000
        val google = FakeGoogle(mapOf("google-a" to listOf(
            Msg("trigger", trigger), Msg("recent", BASE - 1_000), Msg("older", BASE - 2 * HOUR), Msg("oldest", BASE - 3 * HOUR),
        )))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        val result = coordinator(core).resumeConversation(google, "google-a")

        assertEquals(RcsRecoveryResult.COMPLETE, result)
        assertEquals(setOf("trigger", "recent"), core.messages)
        assertEquals(trigger, core.conversation("core-a").watermarkMs)
        assertEquals(2, google.requests.size)
    }

    @Test fun `outage then reconnect recovers every message newer than the watermark`() {
        val messages = listOf(
            Msg("m5", BASE + 50), Msg("m4", BASE + 40), Msg("m3", BASE + 30),
            Msg("m2", BASE + 20), Msg("m1", BASE + 10), Msg("old", BASE - 2 * HOUR),
        )
        val google = FakeGoogle(mapOf("google-a" to messages))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        assertEquals(RcsRecoveryResult.COMPLETE, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(setOf("m1", "m2", "m3", "m4", "m5"), core.messages)
        assertEquals(BASE + 50, core.conversation("core-a").watermarkMs)
    }

    @Test fun `recovery re-reads an unknown group sender once per conversation traversal`() {
        val google = FakeGoogle(emptyMap()).apply { fetchHook = { googleId, _ -> unknownGroupGapPage(googleId) } }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))
        val rereads = AtomicInteger()

        val result = coordinator(core, reconcileGroups = { rereads.incrementAndGet(); true }).resumeConversation(google, "google-a")

        assertEquals(RcsRecoveryResult.COMPLETE, result)
        assertEquals(1, rereads.get())
        assertEquals(2, core.ingestedRecords.size)
    }

    @Test fun `a mixed page ingests only the slack window and nothing older`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(
            Msg("new", BASE + 1), Msg("old1", BASE - 2 * HOUR), Msg("old2", BASE - 3 * HOUR),
        )))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        coordinator(core).resumeConversation(google, "google-a")

        assertEquals(setOf("new"), core.messages)
        assertEquals(1, google.requests.size)
    }

    @Test fun `provider floor mid-page is skipped and the run completes`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 3), Msg("b", BASE + 2), Msg("c", BASE + 1))))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE))).apply { refusals["b"] = "provider_floor" }

        val result = coordinator(core).resumeConversation(google, "google-a")

        assertEquals(RcsRecoveryResult.COMPLETE, result)
        assertEquals(setOf("a", "c"), core.messages)
        assertEquals(BASE + 3, core.conversation("core-a").watermarkMs)
    }

    @Test fun `out-of-order and equal-timestamp records at the watermark boundary are not lost`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(
            Msg("x", BASE + 10), Msg("boundary", BASE), Msg("late", BASE + 20), Msg("twin", BASE),
        )))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        coordinator(core).resumeConversation(google, "google-a")

        assertEquals(setOf("x", "boundary", "late", "twin"), core.messages)
        assertEquals(BASE + 20, core.conversation("core-a").watermarkMs)
    }

    @Test fun `cap hit keeps progress and the next trigger resumes by hint and completes to the original target`() {
        val messages = (0 until 100).map { Msg("m$it", BASE + (100 - it) * 1_000L) }
        val google = FakeGoogle(mapOf("google-a" to messages))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))
        val coordinator = coordinator(core)

        assertEquals(RcsRecoveryResult.INCOMPLETE, coordinator.resumeConversation(google, "google-a"))
        assertEquals(32, google.requests.size)
        assertEquals(BASE, core.conversation("core-a").watermarkMs)
        assertEquals(BASE + 100_000, core.conversation("core-a").hint?.first)

        assertEquals(RcsRecoveryResult.COMPLETE, coordinator.resumeConversation(google, "google-a"))
        assertEquals("o:96", google.requests[32].second)
        assertEquals(34, google.requests.size)
        assertEquals(BASE + 100_000, core.conversation("core-a").watermarkMs)
        assertNull(core.conversation("core-a").hint)
        assertEquals(100, core.messages.size)
        assertEquals(100, core.ingestedRecords.size)
    }

    @Test fun `invalid hint cursor clears the hint, leaves the watermark, and the next trigger restarts from newest`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("new", BASE + 1), Msg("newer", BASE + 2))))
        val conv = conv("core-a", "google-a", watermarkMs = BASE, hint = BASE + 5_000 to "o:999")
        val core = FakeCore(listOf(conv))
        val coordinator = coordinator(core)

        assertEquals(RcsRecoveryResult.INCOMPLETE, coordinator.resumeConversation(google, "google-a"))
        assertEquals(listOf("o:999"), google.requests.map { it.second })
        assertEquals(BASE, conv.watermarkMs)
        assertNull(conv.hint)
        assertTrue(conv.incomplete)
        assertEquals(1, core.clears.get())

        assertEquals(RcsRecoveryResult.COMPLETE, coordinator.resumeConversation(google, "google-a"))
        assertEquals("", google.requests.last().second)
        assertEquals(BASE + 2, conv.watermarkMs)
        assertEquals(setOf("new", "newer"), core.messages)
    }

    @Test fun `a failed page fetch clears the hint and keeps the watermark`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("new", BASE + 1)))).apply {
            fetchHook = { _, _ -> """{"code":"gap_failed"}""" }
        }
        val conv = conv("core-a", "google-a", watermarkMs = BASE)
        val core = FakeCore(listOf(conv))

        assertEquals(RcsRecoveryResult.INCOMPLETE, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(BASE, conv.watermarkMs)
        assertTrue(conv.incomplete)
        assertTrue(core.messages.isEmpty())
    }

    @Test fun `a live event during traversal never moves the watermark or writes recovery state`() {
        val messages = listOf(Msg("m3", BASE + 30), Msg("m2", BASE + 20), Msg("m1", BASE + 10), Msg("m0", BASE + 5))
        val google = FakeGoogle(mapOf("google-a" to messages))
        val conv = conv("core-a", "google-a", watermarkMs = BASE)
        val core = FakeCore(listOf(conv))
        val coordinator = coordinator(core)
        var watermarkDuringLive = -1L
        var writesBeforeLive = -1
        google.fetchHook = { _, cursor ->
            if (cursor == "o:3") {
                writesBeforeLive = core.progressWrites.size
                coordinator.receive(liveRecord("google-a", sentAtMs = BASE + 40, messageId = "live"))
                watermarkDuringLive = conv.watermarkMs
            }
            null
        }

        assertEquals(RcsRecoveryResult.COMPLETE, coordinator.resumeConversation(google, "google-a"))

        assertEquals(BASE, watermarkDuringLive)
        assertEquals("live ingest must not write progress", writesBeforeLive, core.progressWrites.size)
        assertTrue(core.messages.contains("live"))
        assertEquals(1, core.provenances.count { it == NativeRcsObservationProvenance.LIVE })
    }

    @Test fun `live ingest never touches recovery state`() {
        val conv = conv("core-a", "google-a", watermarkMs = BASE)
        val core = FakeCore(listOf(conv))

        coordinator(core).receive(liveRecord("google-a", sentAtMs = BASE + 1))

        assertTrue(core.progressWrites.isEmpty())
        assertTrue(core.completions.isEmpty())
        assertEquals(0, core.clears.get())
        assertEquals(BASE, conv.watermarkMs)
    }

    @Test fun `ingest refusal aborts without progress or watermark change`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 2), Msg("b", BASE + 1)))).apply { pageSize = 1 }
        val conv = conv("core-a", "google-a", watermarkMs = BASE)
        val core = FakeCore(listOf(conv)).apply { refusals["a"] = "identity_conflict" }

        assertRecoveryAbort(259L) { coordinator(core).resumeConversation(google, "google-a") }

        assertTrue(core.progressWrites.isEmpty())
        assertEquals(BASE, conv.watermarkMs)
        assertTrue(core.messages.isEmpty())
    }

    @Test fun `cancellation during a page prevents later records and progress`() {
        val cancelled = AtomicBoolean()
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 3), Msg("b", BASE + 2), Msg("c", BASE + 1))))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE))).apply {
            onIngest = { _, _, _ -> cancelled.set(true); accepted() }
        }

        val result = assertRecoveryAbort(258L) { coordinator(core).resumeConversation(google, "google-a", cancelled) }

        assertEquals(RcsRecoveryResult.ABORTED, result)
        assertEquals(1, core.ingestedRecords.size)
        assertTrue(core.progressWrites.isEmpty())
    }

    @Test fun `history signal superseded by core completes without a core write`() {
        val google = FakeGoogle(emptyMap()).apply {
            fetchHook = { _, _ -> signalPage("google-a", BASE - HOUR) }
        }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE))).apply { invalidateResult = false }

        assertEquals(RcsRecoveryResult.COMPLETE, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(listOf("google-a" to (BASE - HOUR)), core.invalidations)
        assertTrue(core.completions.isEmpty())
    }

    @Test fun `history signal that invalidates aborts immediately without a later write`() {
        val google = FakeGoogle(emptyMap()).apply {
            fetchHook = { _, _ -> signalPage("google-a", BASE + HOUR) }
        }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        assertEquals(RcsRecoveryResult.ABORTED, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(listOf("google-a" to (BASE + HOUR)), core.invalidations)
        assertTrue(core.progressWrites.isEmpty())
        assertTrue(core.completions.isEmpty())
    }

    @Test fun `invalidating history signal records its coarse age before the abort`() {
        val now = BASE + 24 * HOUR
        val cases = listOf(
            null to 266L,
            now + 1 to 267L,
            now - 2 * 60_000 + 1 to 268L,
            now - 2 * 60_000 to 269L,
            now - 15 * 60_000 + 1 to 269L,
            now - HOUR to 271L,
            now - 24 * HOUR to 272L,
        )
        cases.forEach { (sentAtMs, expectedAge) ->
            val records = mutableListOf<String>()
            val google = FakeGoogle(emptyMap()).apply { fetchHook = { _, _ -> signalPage("google-a", sentAtMs) } }
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
                assertEquals(RcsRecoveryResult.ABORTED, coordinator(FakeCore(listOf(conv("core-a", "google-a"))), nowMs = { now })
                    .resumeConversation(google, "google-a"))
            }
            assertEquals(listOf(expectedAge, 256L, 44L), records.map { it.substringAfter("29=").toLong() })
        }
    }

    @Test fun `superseded history signal records its coarse skipped age`() {
        val now = BASE + HOUR
        val records = mutableListOf<String>()
        val core = FakeCore(listOf(conv("core-a", "google-a"))).apply { invalidateResult = false }
        val google = FakeGoogle(emptyMap()).apply { fetchHook = { _, _ -> signalPage("google-a", now - HOUR) } }

        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            assertEquals(RcsRecoveryResult.COMPLETE, coordinator(core, nowMs = { now }).resumeConversation(google, "google-a"))
        }

        assertEquals(listOf(278L, 255L), records.map { it.substringAfter("29=").toLong() })
    }

    @Test fun `history signals for one conversation stay ordered and retain their times`() {
        val google = FakeGoogle(emptyMap()).apply {
            fetchHook = { _, _ -> signalPage("google-a", BASE - HOUR, BASE + HOUR) }
        }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE))).apply {
            invalidateResults += listOf(false, true)
        }

        assertEquals(RcsRecoveryResult.ABORTED, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(listOf("google-a" to (BASE - HOUR), "google-a" to (BASE + HOUR)), core.invalidations)
    }

    @Test fun `history signal without sent time is passed as absent`() {
        val google = FakeGoogle(emptyMap()).apply {
            fetchHook = { _, _ -> signalPage("google-a", null) }
        }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))

        assertEquals(RcsRecoveryResult.ABORTED, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(listOf("google-a" to null), core.invalidations)
    }

    @Test fun `group-lane history signal failure records its abort cause`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 2), Msg("b", BASE + 1)))).apply {
            fetchHook = { _, _ ->
                JSONObject().put("code", "ok").put("records", JSONArray())
                    .put("signals", JSONArray().put(JSONObject().put("google_conversation_id", "google-a")).put(JSONObject().put("google_conversation_id", "google-a")))
                    .put("next_cursor", "next").put("has_more", true).toString()
            }
        }
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE))).apply {
            invalidateFailure = IllegalStateException("stale expectation")
        }

        assertRecoveryAbort(260L) { coordinator(core).resumeConversation(google, "google-a") }

        assertEquals(1, core.invalidations.size)
        assertTrue(core.progressWrites.isEmpty())
    }

    @Test fun `a looping cursor is a failure, not an endless traversal`() {
        val google = FakeGoogle(emptyMap()).apply {
            fetchHook = { _, _ ->
                JSONObject().put("code", "ok").put("records", JSONArray().put(JSONObject()
                    .put("google_message_id", "n").put("google_conversation_id", "google-a").put("sent_at_ms", BASE + 1)))
                    .put("signals", JSONArray()).put("next_cursor", "loop").put("has_more", true).toString()
            }
        }
        val conv = conv("core-a", "google-a", watermarkMs = BASE)
        val core = FakeCore(listOf(conv))

        assertEquals(RcsRecoveryResult.INCOMPLETE, coordinator(core).resumeConversation(google, "google-a"))

        assertEquals(2, google.requests.size)
        assertEquals(BASE, conv.watermarkMs)
        assertNull(conv.hint)
    }

    @Test fun `failed progress or context reload aborts recovery`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 1), Msg("b", BASE + 2)))).apply { pageSize = 1 }
        val progressFailure = FakeCore(listOf(conv("core-a", "google-a"))).apply { progressRejection = "stale" }
        assertRecoveryAbort(261L) { coordinator(progressFailure).resumeConversation(google, "google-a") }

        val reloadMissing = FakeCore(listOf(conv("core-a", "google-a"))).apply { contextMissingAfterProgress = true }
        assertRecoveryAbort(262L) { coordinator(reloadMissing).resumeConversation(google, "google-a") }
    }

    @Test fun `failed finish or hint clearing aborts recovery`() {
        val completionFailure = FakeCore(listOf(conv("core-a", "google-a"))).apply { completionRejection = "stale" }
        assertRecoveryAbort(263L) {
            coordinator(completionFailure).resumeConversation(FakeGoogle(mapOf("google-a" to listOf(Msg("a", BASE + 1)))), "google-a")
        }

        val clearFailure = FakeCore(listOf(conv("core-a", "google-a"))).apply { clearRejection = "stale" }
        val badPage = FakeGoogle(emptyMap()).apply { fetchHook = { _, _ -> """{\"code\":\"failed\"}""" } }
        assertRecoveryAbort(264L) { coordinator(clearFailure).resumeConversation(badPage, "google-a") }
    }

    @Test fun `missing initial context records only its abort cause`() {
        assertRecoveryAbort(257L, recoveryAbort = false) {
            coordinator(FakeCore(emptyList())).resumeConversation(FakeGoogle(emptyMap()), "google-a")
        }
    }

    @Test fun `idle conversations are skipped by last message time but zero or missing activity is swept`() {
        val google = FakeGoogle(emptyMap()).apply {
            activityJson = JSONObject().put("code", "ok").put("complete", true)
                .put("last_message_ms", JSONObject().put("google-idle", BASE - 2 * HOUR).put("google-zero", 0L)).toString()
        }
        val core = FakeCore(listOf(
            conv("core-idle", "google-idle", watermarkMs = BASE),
            conv("core-zero", "google-zero", watermarkMs = BASE),
            conv("core-missing", "google-missing", watermarkMs = BASE),
        ))

        coordinator(core).resume(google)

        assertEquals(listOf("google-zero", "google-missing"), google.requests.map { it.first })
    }

    @Test fun `sweep recovers at most twenty five conversations and the next sweep continues and wraps`() {
        val convs = (0 until 30).map { conv("core-$it", "google-$it") }
        val google = FakeGoogle(emptyMap())
        val coordinator = coordinator(FakeCore(convs))

        coordinator.resume(google)
        assertEquals((0 until 25).map { "google-$it" }, google.requests.map { it.first })

        coordinator.resume(google)
        val second = google.requests.drop(25).map { it.first }
        assertEquals((25 until 30).map { "google-$it" } + (0 until 20).map { "google-$it" }, second)
    }

    @Test fun `unknown context requests a targeted bootstrap for every live event`() {
        val requests = mutableListOf<String>()
        val coordinator = coordinator(FakeCore(emptyList()), bootstrapRequest = requests::add)

        coordinator.receive(liveRecord("google-new", sentAtMs = 2_000L))
        coordinator.receive(liveRecord("google-new", sentAtMs = 1_000L))
        coordinator.receive(liveRecord("google-other", sentAtMs = 3_000L))

        assertEquals(listOf("google-new", "google-new", "google-other"), requests)
    }

    @Test fun `a live record refused because the proof was withdrawn requests a new proof`() {
        val requests = mutableListOf<String>()
        val core = FakeCore(listOf(conv("core-a", "google-a")))
        core.refusals["message-1"] = "conversation_unproven"

        coordinator(core, bootstrapRequest = requests::add).receive(liveRecord("google-a"))

        assertEquals(listOf("google-a"), requests)
    }

    @Test fun `unknown group sender re-reads the snapshot once and ingests the record unattributed`() {
        val core = FakeCore(listOf(conv("core-a", "google-a")))
        val rereads = AtomicInteger()

        coordinator(core, reconcileGroups = { rereads.incrementAndGet(); true }).receive(groupRecord("google-a", senderUnknown = true))

        assertEquals(1, rereads.get())
        assertEquals(1, core.ingestedRecords.size)
    }

    @Test fun `attributed group sender and one-to-one records never trigger a snapshot re-read`() {
        val core = FakeCore(listOf(conv("core-a", "google-a")))
        val rereads = AtomicInteger()
        val coordinator = coordinator(core, reconcileGroups = { rereads.incrementAndGet(); true })

        coordinator.receive(groupRecord("google-a", senderAddress = "+12025550102"))
        coordinator.receive(liveRecord("google-a"))

        assertEquals(0, rereads.get())
        assertEquals(2, core.ingestedRecords.size)
    }

    @Test fun `unmapped group record reconciles once, retries the context, and skips the targeted bootstrap`() {
        val core = FakeCore(emptyList())
        val requests = mutableListOf<String>()
        val reconciles = AtomicInteger()
        val coordinator = coordinator(core, bootstrapRequest = requests::add, reconcileGroups = {
            reconciles.incrementAndGet()
            core.addConversation(conv("core-g", "google-g"))
            true
        })

        coordinator.receive(groupRecord("google-g", senderAddress = "+12025550102"))

        assertEquals(1, reconciles.get())
        assertTrue(requests.isEmpty())
        assertEquals(1, core.ingestedRecords.size)
    }

    @Test fun `unmapped record still unmapped after the one reconcile falls back to the targeted bootstrap`() {
        val core = FakeCore(emptyList())
        val requests = mutableListOf<String>()
        val reconciles = AtomicInteger()

        coordinator(core, bootstrapRequest = requests::add, reconcileGroups = { reconciles.incrementAndGet(); true })
            .receive(groupRecord("google-missing", senderAddress = "+12025550102"))

        assertEquals(1, reconciles.get())
        assertEquals(listOf("google-missing"), requests)
        assertTrue(core.ingestedRecords.isEmpty())
    }

    @Test fun `live protocol switch invalidates its current context without ingestion`() {
        val core = FakeCore(listOf(conv("core-a", "google-a")))

        coordinator(core).receive(JSONObject().put("code", "protocol_switch").put("google_conversation_id", "google-a").toString())

        assertEquals(listOf("google-a" to null), core.invalidations)
        assertTrue(core.ingestedRecords.isEmpty())
    }

    @Test fun `live protocol switch records invalidation but no signal age`() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            coordinator(FakeCore(listOf(conv("core-a", "google-a")))).receive(
                JSONObject().put("code", "protocol_switch").put("google_conversation_id", "google-a").toString(),
            )
        }
        assertTrue(records.none { it.substringAfter("29=").toLong() in 266L..279L })
        assertEquals(listOf(265L), records.map { it.substringAfter("29=").toLong() })
    }

    @Test fun `live protocol switch without invalidation records no diagnostics`() {
        val records = mutableListOf<String>()
        val core = FakeCore(listOf(conv("core-a", "google-a"))).apply { invalidateResult = false }
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            coordinator(core).receive(JSONObject().put("code", "protocol_switch").put("google_conversation_id", "google-a").toString())
        }
        assertTrue(records.isEmpty())
    }

    @Test fun `live and gap ingestion preserve acquisition provenance`() {
        val google = FakeGoogle(mapOf("google-a" to listOf(Msg("gap", BASE + 1))))
        val core = FakeCore(listOf(conv("core-a", "google-a", watermarkMs = BASE)))
        val coordinator = coordinator(core)
        coordinator.receive(liveRecord("google-a", messageId = "live"))
        coordinator.resume(google)
        assertEquals(listOf(NativeRcsObservationProvenance.LIVE, NativeRcsObservationProvenance.HISTORICAL), core.provenances)
    }

    @Test fun `live Go envelope ingests only its inner record`() {
        val core = FakeCore(listOf(conv("core-a", "google-a")))

        coordinator(core).receive(liveRecord("google-a"))

        assertEquals(1, core.ingestedRecords.size)
        assertEquals("google-a", JSONObject(core.ingestedRecords.single()).getString("google_conversation_id"))
        assertEquals(null, JSONObject(core.ingestedRecords.single()).opt("code"))
        assertEquals(listOf(NativeRcsObservationProvenance.LIVE), core.provenances)
    }

    private fun coordinator(
        core: RcsReceiveCore,
        enabled: Boolean = true,
        bootstrapRequest: (String) -> Unit = {},
        reconcileGroups: () -> Boolean = { false },
        nowMs: () -> Long = System::currentTimeMillis,
    ) = RcsReceiveCoordinator(
        ApplicationProvider.getApplicationContext(),
        core,
        { enabled },
        bootstrapRequest,
        reconcileGroups,
        nowMs,
    )

    private fun assertRecoveryAbort(code: Long, recoveryAbort: Boolean = true, action: () -> RcsRecoveryResult): RcsRecoveryResult {
        val records = mutableListOf<String>()
        val result = SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) { action() }
        assertEquals(RcsRecoveryResult.ABORTED, result)
        val reasons = records.map { it.substringAfter("29=").toLong() }
        assertEquals(if (recoveryAbort) listOf(code, 44L) else listOf(code), reasons)
        return result
    }

    private fun thread(block: () -> Unit) = Thread(block).apply { start() }

    private fun waitUntilEntered(active: ConcurrentHashMap<String, AtomicInteger>, conversationId: String): Boolean {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (System.nanoTime() < deadline) {
            if ((active[conversationId]?.get() ?: 0) > 0) return true
            Thread.yield()
        }
        return false
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsReceiveCoordinatorGatewaySmokeTest : GatewayTestBase() {
    @Test fun `generated projected observation binding returns fixed ownership refusal on a real store`() {
        val client = enrollAndUnlock()
        val observation = JSONObject()
            .put("google_message_id", "synthetic-message")
            .put("google_conversation_id", "synthetic-conversation")
            .put("direction", "incoming")
            .put("body", "synthetic body")
            .put("sent_at_ms", 1_735_689_600_123L)
            .put("status", "")
            .put("rcs_reported", false)
            .toString()
        val outcome = client.gatewayIngestRcsProjectedObservation(
            UUID.randomUUID().toString(),
            1u,
            observation,
            NativeRcsObservationProvenance.LIVE,
        )
        assertEquals("rejected:conversation_unproven", outcome.recordOutcome)
        assertEquals(null, outcome.statusOutcome)
    }
}

/** Per-conversation recovery state, mirroring the durable watermark, hint and gap flag. */
private class Conv(
    val conversationId: String,
    val googleId: String,
    var watermarkMs: Long,
    var hint: Pair<Long, String>?,
    var incomplete: Boolean,
)

private fun conv(
    conversationId: String,
    googleId: String,
    watermarkMs: Long = BASE,
    hint: Pair<Long, String>? = null,
    incomplete: Boolean = false,
) = Conv(conversationId, googleId, watermarkMs, hint, incomplete)

private class FakeCore(initial: List<Conv>) : RcsReceiveCore {
    private val conversations = initial.toMutableList()
    /** A snapshot re-read can link or create a group; the fake models that by adding its context. */
    fun addConversation(conversation: Conv) { conversations += conversation }

    var failOnUse = false
    var contextsFailure: Throwable? = null
    val contextsCalls = AtomicInteger()
    var progressRejection: String? = null
    var completionRejection: String? = null
    var clearRejection: String? = null
    var contextMissingAfterProgress = false
    private var contextAvailable = true
    var invalidateFailure: Throwable? = null
    var invalidateResult = true
    val invalidateResults = ArrayDeque<Boolean>()
    var onIngest: ((NativeRcsReceiveContext, String, NativeRcsObservationProvenance) -> NativeRcsIngestOutcome)? = null
    val refusals = ConcurrentHashMap<String, String>()
    val messages: MutableSet<String> = ConcurrentHashMap.newKeySet()
    val ingestedRecords = Collections.synchronizedList(mutableListOf<String>())
    val provenances = Collections.synchronizedList(mutableListOf<NativeRcsObservationProvenance>())
    val progressWrites = Collections.synchronizedList(mutableListOf<Triple<String, Long, String>>())
    val completions = Collections.synchronizedList(mutableListOf<Pair<String, Long>>())
    val clears = AtomicInteger()
    val invalidations = Collections.synchronizedList(mutableListOf<Pair<String, Long?>>())

    fun conversation(conversationId: String): Conv = conversations.single { it.conversationId == conversationId }

    override fun context(googleId: String): NativeRcsReceiveContext? = used {
        if (contextAvailable) conversations.firstOrNull { it.googleId == googleId }?.let(::snapshot) else null
    }

    override fun contexts(): List<NativeRcsReceiveContext> = used {
        contextsCalls.incrementAndGet()
        contextsFailure?.let { throw it }
        conversations.map(::snapshot)
    }

    override fun ingest(
        context: NativeRcsReceiveContext,
        record: String,
        provenance: NativeRcsObservationProvenance,
    ): NativeRcsIngestOutcome = used {
        ingestedRecords += record
        provenances += provenance
        onIngest?.let { return@used it(context, record, provenance) }
        val id = JSONObject(record).getString("google_message_id")
        refusals[id]?.let { return@used NativeRcsIngestOutcome("rejected:$it", null) }
        if (messages.add(id)) accepted() else NativeRcsIngestOutcome("duplicate", null)
    }

    override fun recordProgress(context: NativeRcsReceiveContext, targetMs: Long, cursor: String): String = used {
        progressWrites += Triple(context.conversationId, targetMs, cursor)
        progressRejection?.let { return@used "rejected:$it" }
        if (contextMissingAfterProgress) contextAvailable = false
        conversation(context.conversationId).apply { hint = targetMs to cursor; incomplete = true }
        "recorded"
    }

    override fun complete(context: NativeRcsReceiveContext, targetMs: Long): String = used {
        completions += context.conversationId to targetMs
        completionRejection?.let { return@used "rejected:$it" }
        conversation(context.conversationId).apply {
            watermarkMs = maxOf(watermarkMs, targetMs)
            hint = null
            incomplete = false
        }
        "recorded"
    }

    override fun clearHint(context: NativeRcsReceiveContext): String = used {
        clears.incrementAndGet()
        clearRejection?.let { return@used "rejected:$it" }
        conversation(context.conversationId).apply { hint = null; incomplete = true }
        "recorded"
    }

    override fun invalidate(context: NativeRcsReceiveContext, googleId: String, eventAtMs: Long?): Boolean = used {
            invalidations += googleId to eventAtMs
            invalidateFailure?.let { throw it }
            return@used if (invalidateResults.isEmpty()) invalidateResult else invalidateResults.removeFirst()
    }

    private fun snapshot(conv: Conv) = NativeRcsReceiveContext(
        conv.conversationId,
        conv.googleId,
        7u,
        11u,
        NativeRcsReceiveOwner.LINK,
        conv.watermarkMs,
        conv.hint?.let { NativeRcsResumeHint(it.first, it.second) },
        conv.incomplete,
        NativeRcsContextExpectation(7u, 13u, 17u, 19u),
    )

    private fun <T> used(block: () -> T): T {
        check(!failOnUse) { "core must not be used" }
        return block()
    }
}

private data class Msg(val id: String, val sentAtMs: Long)

/** Serves newest-first backward pages with opaque offset cursors, like the Go gap fetch. */
private class FakeGoogle(
    private val messages: Map<String, List<Msg>>,
    var pageSize: Int = 3,
) : LinkHandle {
    val requests = Collections.synchronizedList(mutableListOf<Pair<String, String>>())
    var fetchHook: ((String, String) -> String?)? = null
    var activityJson: String? = null

    override fun fetchRcsGap(googleConversationId: String, pageToken: String): String {
        requests += googleConversationId to pageToken
        fetchHook?.invoke(googleConversationId, pageToken)?.let { return it }
        val list = messages[googleConversationId].orEmpty()
        val offset = when {
            pageToken.isEmpty() -> 0
            pageToken.startsWith("o:") -> pageToken.removePrefix("o:").toIntOrNull() ?: return INVALID_CURSOR
            else -> return INVALID_CURSOR
        }
        if (offset !in 0..list.size) return INVALID_CURSOR
        val end = minOf(list.size, offset + pageSize)
        val records = JSONArray(list.subList(offset, end).map { record(googleConversationId, it) })
        return JSONObject()
            .put("code", "ok")
            .put("records", records)
            .put("signals", JSONArray())
            .put("next_cursor", "o:$end")
            .put("has_more", end < list.size)
            .put("skipped", 0)
            .toString()
    }

    override fun conversationLastActivity(googleIdsJson: String): String =
        activityJson ?: super.conversationLastActivity(googleIdsJson)

    override fun pair(cookiesJson: ByteArray) = Unit
    override fun relink() = Unit
    override fun run() = Unit
    override fun checkHealth() = 0L
    override fun unlink() = Unit
    override fun stop() = Unit
    override fun close() = Unit
    override fun status() = "{}"

    private fun record(googleId: String, message: Msg) = JSONObject()
        .put("google_message_id", message.id)
        .put("google_conversation_id", googleId)
        .put("direction", "incoming")
        .put("body", "synthetic body")
        .put("sent_at_ms", message.sentAtMs)
        .put("status", "delivered")
        .put("tmp_id", "tmp-${message.id}")
        .put("rcs_reported", true)

    private companion object {
        const val INVALID_CURSOR = """{"code":"cursor_invalid"}"""
    }
}

private fun accepted() = NativeRcsIngestOutcome("inserted", "status_applied")

private fun signalPage(googleId: String, vararg sentAtMs: Long?) = JSONObject()
    .put("code", "ok")
    .put("records", JSONArray())
    .put("signals", JSONArray(sentAtMs.map { time ->
        JSONObject().put("google_conversation_id", googleId).apply { time?.let { put("sent_at_ms", it) } }
    }))
    .put("has_more", false)
    .toString()

/** Two group gap records with unresolved senders, in one page that completes the traversal. */
private fun unknownGroupGapPage(googleId: String) = JSONObject()
    .put("code", "ok")
    .put("records", JSONArray((1..2).map { index ->
        JSONObject()
            .put("google_message_id", "group-$index")
            .put("google_conversation_id", googleId)
            .put("direction", "incoming")
            .put("body", "synthetic body")
            .put("sent_at_ms", BASE + index)
            .put("status", "delivered")
            .put("tmp_id", "tmp-$index")
            .put("rcs_reported", true)
            .put("sender_unknown", true)
    }))
    .put("signals", JSONArray())
    .put("has_more", false)
    .toString()

/** A group Live record as Go projects it: an attributed sender, or `sender_unknown` with no address. */
private fun groupRecord(googleId: String, senderAddress: String? = null, senderUnknown: Boolean = false) = JSONObject()
    .put("code", "ok")
    .put("record", JSONObject()
        .put("google_message_id", "group-message-1")
        .put("google_conversation_id", googleId)
        .put("direction", "incoming")
        .put("body", "synthetic body")
        .put("sent_at_ms", BASE)
        .put("status", "delivered")
        .put("tmp_id", "tmp-1")
        .put("rcs_reported", true)
        .apply {
            senderAddress?.let { put("sender_address", it) }
            if (senderUnknown) put("sender_unknown", true)
        })
    .toString()

private fun liveRecord(googleId: String, sentAtMs: Long = BASE, messageId: String = "message-1") = JSONObject()
    .put("code", "ok")
    .put("record", JSONObject()
        .put("google_message_id", messageId)
        .put("google_conversation_id", googleId)
        .put("direction", "incoming")
        .put("body", "synthetic body")
        .put("sent_at_ms", sentAtMs)
        .put("status", "delivered")
        .put("tmp_id", "tmp-1")
        .put("rcs_reported", true))
    .toString()
