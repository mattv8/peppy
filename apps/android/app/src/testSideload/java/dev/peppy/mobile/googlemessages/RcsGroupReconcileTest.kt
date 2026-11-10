package dev.peppy.mobile.googlemessages

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeGroupClassification
import uniffi.peppy_mobile_bindings.NativeGroupCreateBegin
import uniffi.peppy_mobile_bindings.NativeGroupCreateOutcome
import uniffi.peppy_mobile_bindings.NativePermitState

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsGroupReconcileTest {
    @Test fun `a complete snapshot is ingested once and an incomplete one never reaches core`() {
        val port = FakeIngest()
        val reconciler = RcsGroupReconciler(port, FixedClock)

        assertTrue(reconciler.reconcile(snapshot(complete = true)))
        assertEquals(1, port.snapshots.size)

        assertEquals(false, reconciler.reconcile(snapshot(complete = false)))
        assertEquals(false, reconciler.reconcile("{\"code\":\"conversation_candidate_snapshot_unavailable\"}"))
        assertEquals(1, port.snapshots.size)
    }

    @Test fun `a core refusal reports failure without throwing`() {
        val port = FakeIngest().apply { failure = IllegalStateException("invalid") }
        assertEquals(false, RcsGroupReconciler(port, FixedClock).reconcile(snapshot(complete = true)))
    }

    @Test fun `a permitted create runs Go once with canonical numbers and the name, then commits and reconciles`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), "Weekend"))
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}")
        val reconciles = mutableListOf<Unit>()
        val outcome = RcsGroupCreateCoordinator(core, go::create, { reconciles += Unit; true }, FixedClock).create("conversation", "command")

        assertEquals(NativeGroupCreateOutcome.PENDING, outcome)
        assertEquals(1, go.calls.size)
        val request = JSONObject(go.calls.single())
        assertEquals(listOf("+12025550101", "+12025550102"), request.getJSONArray("numbers").toStrings())
        assertEquals("Weekend", request.getString("name"))
        assertEquals(listOf("command" to "{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}"), core.commits)
        assertEquals(1, reconciles.size)
    }

    @Test fun `not_rcs commits the answer and never retries the Go create`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), null))
        val go = FakeGoCreate("{\"code\":\"not_rcs\"}")

        val outcome = RcsGroupCreateCoordinator(core, go::create, { true }, FixedClock).create("conversation", "command")

        assertEquals(NativeGroupCreateOutcome.NOT_RCS, outcome)
        assertEquals(1, go.calls.size)
        assertEquals(listOf("command" to "{\"code\":\"not_rcs\"}"), core.commits)
        assertTrue(!JSONObject(go.calls.single()).has("name"))
    }

    @Test fun `a thrown or silent Go create commits unknown exactly once and reconciles`() {
        listOf<(String) -> String?>({ throw IllegalStateException("link") }, { null }).forEach { call ->
            val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), null))
            var reconciles = 0
            val outcome = RcsGroupCreateCoordinator(core, call, { reconciles++; true }, FixedClock).create("conversation", "command")
            assertEquals(NativeGroupCreateOutcome.UNKNOWN, outcome)
            assertEquals(listOf("command" to "{\"code\":\"unknown\"}"), core.commits)
            assertEquals(1, reconciles)
        }
    }

    @Test fun `a result commit that fails after Go answered settles the create unknown once and never repeats Go`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), "Weekend"))
        core.commitFailure = IllegalStateException("commit failed")
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}")
        var reconciles = 0

        val outcome = RcsGroupCreateCoordinator(core, go::create, { reconciles++; true }, FixedClock).create("conversation", "command")

        assertEquals(NativeGroupCreateOutcome.UNKNOWN, outcome)
        assertEquals(1, go.calls.size)
        assertEquals(listOf("command"), core.settles)
        assertEquals(1, reconciles)
    }

    @Test fun `a settlement that also fails is attempted once, keeps the outcome unknown, and never repeats Go`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), "Weekend"))
        core.commitFailure = IllegalStateException("commit failed")
        core.settleFailure = IllegalStateException("settle failed")
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}")

        val outcome = RcsGroupCreateCoordinator(core, go::create, { true }, FixedClock).create("conversation", "command")

        assertEquals(NativeGroupCreateOutcome.UNKNOWN, outcome)
        assertEquals(1, go.calls.size)
        assertEquals(listOf("command"), core.settles)
    }

    @Test fun `a committed answer is never settled`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+12025550101", "+12025550102"), "Weekend"))
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}")

        assertEquals(NativeGroupCreateOutcome.PENDING, RcsGroupCreateCoordinator(core, go::create, { true }, FixedClock).create("conversation", "command"))
        assertTrue(core.settles.isEmpty())
    }

    @Test fun `the create request carries the name only when named, which the Go decoder accepts`() {
        val named = JSONObject(groupCreateRequestJson(listOf("+15551230001", "+15551230002"), "Weekend"))
        assertEquals(setOf("numbers", "name"), named.keys().asSequence().toSet())
        assertEquals("Weekend", named.getString("name"))
        assertEquals(listOf("+15551230001", "+15551230002"), named.getJSONArray("numbers").toStrings())

        val unnamed = JSONObject(groupCreateRequestJson(listOf("+15551230001", "+15551230002"), null))
        assertEquals(setOf("numbers"), unnamed.keys().asSequence().toSet())
        assertEquals(listOf("+15551230001", "+15551230002"), unnamed.getJSONArray("numbers").toStrings())
    }

    @Test fun `an unnamed permitted create sends no name to Go`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+15551230001", "+15551230002"), null))
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\",\"google_conversation_id\":\"g\"}")
        assertEquals(NativeGroupCreateOutcome.PENDING, RcsGroupCreateCoordinator(core, go::create, { true }, FixedClock).create("conversation", "command"))
        assertEquals(setOf("numbers"), JSONObject(go.calls.single()).keys().asSequence().toSet())
    }

    @Test fun `a failed reconcile after the create still commits the answer and never retries it`() {
        val core = FakeCreate(NativeGroupCreateBegin.Permit("command", listOf("+15551230001", "+15551230002"), "Weekend"))
        val go = FakeGoCreate("{\"code\":\"unknown\"}")
        assertEquals(NativeGroupCreateOutcome.UNKNOWN, RcsGroupCreateCoordinator(core, go::create, { false }, FixedClock).create("conversation", "command"))
        assertEquals(1, go.calls.size)
        assertEquals(listOf("command" to "{\"code\":\"unknown\"}"), core.commits)
    }

    @Test fun `a refused or non-permit begin runs no Go create and reports nothing`() {
        val refused = FakeCreate(NativeGroupCreateBegin.Refused(NativePermitState.GROUP_CREATE_UNCONFIRMED))
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\"}")
        assertNull(RcsGroupCreateCoordinator(refused, go::create, { true }, FixedClock).create("conversation", "command"))
        assertEquals(0, go.calls.size)
        assertEquals(emptyList<Pair<String, String>>(), refused.commits)
    }

    @Test fun `a begin that core throws for an ineligible 1 to 1 or already-attempted command runs nothing`() {
        val core = FakeCreate(null)
        val go = FakeGoCreate("{\"code\":\"created_rcs_group\"}")
        assertNull(RcsGroupCreateCoordinator(core, go::create, { true }, FixedClock).create("conversation", "command"))
        assertEquals(0, go.calls.size)
    }

    private fun snapshot(complete: Boolean) = JSONObject()
        .put("code", "conversation_candidate_snapshot")
        .put("complete", complete)
        .put("snapshot_token", "1:1:1")
        .put("conversations", JSONArray())
        .toString()

    private fun JSONArray.toStrings() = (0 until length()).map { getString(it) }

    private object FixedClock : RcsBootstrapClock {
        override fun nowMs() = 1_000L
    }

    private class FakeIngest : RcsGroupIngestPort {
        val snapshots = mutableListOf<String>()
        var failure: Throwable? = null
        override fun ingest(snapshotJson: String, nowMs: Long): NativeGroupClassification {
            failure?.let { throw it }
            snapshots += snapshotJson
            return NativeGroupClassification(0uL, 0uL, 0uL, 0uL, 0uL, 0uL, 0uL, 0uL, 0uL)
        }
    }

    private class FakeCreate(private val begin: NativeGroupCreateBegin?) : RcsGroupCreateCore {
        val commits = mutableListOf<Pair<String, String>>()
        val settles = mutableListOf<String>()
        /** When set, the durable result commit throws after Google answered. */
        var commitFailure: Throwable? = null
        /** When set, the settlement throws too, so core keeps it retained. */
        var settleFailure: Throwable? = null
        override fun begin(conversationId: String, commandId: String, nowMs: Long): NativeGroupCreateBegin =
            begin ?: throw IllegalStateException("group create not permitted")
        override fun commit(commandId: String, resultJson: String): NativeGroupCreateOutcome {
            commits += commandId to resultJson
            commitFailure?.let { throw it }
            return if (resultJson.contains("created_rcs_group")) NativeGroupCreateOutcome.PENDING
                else if (resultJson.contains("not_rcs")) NativeGroupCreateOutcome.NOT_RCS
                else NativeGroupCreateOutcome.UNKNOWN
        }
        override fun settle(commandId: String) {
            settles += commandId
            settleFailure?.let { throw it }
        }
    }

    private class FakeGoCreate(private val reply: String?) {
        val calls = mutableListOf<String>()
        fun create(request: String): String? {
            calls += request
            return reply
        }
    }
}
