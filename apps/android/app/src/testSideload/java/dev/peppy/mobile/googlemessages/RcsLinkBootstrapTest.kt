package dev.peppy.mobile.googlemessages

import dev.peppy.mobile.diagnostics.SyncDiagnostics
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeRcsBootstrapResolution
import uniffi.peppy_mobile_bindings.NativeRcsLinkOwnershipClaimOutcome
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerification
import uniffi.peppy_mobile_bindings.NativeRcsMappingVerdict

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsLinkBootstrapTest {
    @Test fun `ready run resolves observes verifies claims publishes and sweeps in order`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001")))

        fixture.bootstrap().run()

        assertEquals(
            listOf("snapshot", "lifecycle", "owned", "resolve:google-a", "tuple", "revision:conv-google-a", "observe:google-a", "verify", "claim:google-a", "sweep:google-a", "publish:[conv-google-a]"),
            fixture.events,
        )
        val claim = JSONObject(fixture.claims.single())
        assertFalse(claim.has("historyCursor"))
        assertFalse(claim.has("cursorObservedAtMs"))
        assertEquals(5L, claim.getLong("expectedLifecycleGeneration"))
        assertEquals(3L, claim.getLong("mappingRevision"))
        assertFalse(claim.has("backfillFromMs"))
    }

    @Test fun `bootstrap records resolution verification and claim diagnostics`() {
        fun reasons(fixture: Fixture): List<String> {
            val records = mutableListOf<String>()
            SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) { fixture.bootstrap().run() }
            return records.map { it.substringAfterLast("29=") }
        }
        val one = listOf(candidate("google-a", "+15550000001"))

        assertEquals(listOf("186"), reasons(Fixture(candidates = one, complete = false)))
        assertEquals(listOf("234"), reasons(Fixture(candidates = one, resolution = { NativeRcsBootstrapResolution.Missing })))
        assertEquals(listOf("235"), reasons(Fixture(candidates = one, resolution = { NativeRcsBootstrapResolution.Ambiguous })))
        assertEquals(listOf("236"), reasons(Fixture(candidates = one, resolution = { NativeRcsBootstrapResolution.Uncertain })))
        assertEquals(listOf("237"), reasons(Fixture(candidates = one, resolution = { NativeRcsBootstrapResolution.InvalidIdentity })))
        assertEquals(listOf("238"), reasons(Fixture(candidates = one, resolution = { NativeRcsBootstrapResolution.AlreadyMappedElsewhere })))
        assertEquals(listOf("194"), reasons(Fixture(candidates = one).apply { observeFailure = setOf("google-a") }))
        assertEquals(listOf("240", "196"), reasons(Fixture(candidates = one).apply {
            verifyOutcome = { NativeRcsMappingVerdict(NativeRcsMappingVerification.REFUSED, "stale_observation") }
        }))
        assertEquals(listOf("196"), reasons(Fixture(candidates = one).apply {
            verifyOutcome = { NativeRcsMappingVerdict(NativeRcsMappingVerification.REFUSED, null) }
        }))
        assertEquals(listOf("197"), reasons(Fixture(candidates = one).apply { claimFailure = setOf("google-a") }))
        assertEquals(listOf("239"), reasons(Fixture(candidates = one)))
    }

    @Test fun `owned conversations are skipped without consuming the per run cap`() {
        val owned = (0 until 50).map { "owned-$it" }
        val fresh = (0 until 45).map { "fresh-$it" }
        val fixture = Fixture(
            candidates = (owned + fresh).mapIndexed { index, id -> candidate(id, "+1555000%04d".format(index)) },
            owned = owned.toSet(),
        )

        fixture.bootstrap().run()

        assertEquals(40, fixture.resolved.size)
        assertEquals(fresh.take(40), fixture.resolved)
    }

    @Test fun `next run continues after the last attempted candidate and wraps`() {
        val ids = (0 until 45).map { "google-$it" }
        val fixture = Fixture(
            candidates = ids.mapIndexed { index, id -> candidate(id, "+1555000%04d".format(index)) },
            resolution = { NativeRcsBootstrapResolution.Missing },
        )
        val bootstrap = fixture.bootstrap()

        bootstrap.run()
        fixture.resolved.clear()
        bootstrap.run()

        assertEquals(ids.drop(40) + ids.take(35), fixture.resolved)
    }

    @Test fun `refusals and failures at every step are contained and later candidates still claim`() {
        val fixture = Fixture(
            candidates = listOf(
                candidate("missing", "+15550000001"),
                candidate("observe-fails", "+15550000002"),
                candidate("verify-refused", "+15550000003"),
                candidate("claim-throws", "+15550000004"),
                candidate("good", "+15550000005"),
            ),
            resolution = { id -> if (id == "missing") NativeRcsBootstrapResolution.Ambiguous else NativeRcsBootstrapResolution.Resolved("conv-$id") },
        ).apply {
            observeFailure = setOf("observe-fails")
            verifyOutcome = { proof -> NativeRcsMappingVerdict(if (proof == "proof-verify-refused") NativeRcsMappingVerification.REFUSED else NativeRcsMappingVerification.MAPPED, null) }
            claimFailure = setOf("claim-throws")
        }

        fixture.bootstrap().run()

        assertEquals(listOf("conv-good"), fixture.published.single())
        assertEquals(listOf("good"), fixture.swept)
    }

    @Test fun `full bootstrap remains transient when any candidate is transient`() {
        val fixture = Fixture(
            candidates = listOf(candidate("uncertain", "+15550000001"), candidate("good", "+15550000002")),
            resolution = { id -> if (id == "uncertain") NativeRcsBootstrapResolution.Uncertain else NativeRcsBootstrapResolution.Resolved("conv-$id") },
        )

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run())
        assertEquals(listOf("conv-good"), fixture.published.single())
    }

    @Test fun `full run whose claimed recovery is incomplete is transient`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001"))).apply {
            recovery = { RcsRecoveryResult.INCOMPLETE }
        }

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run())
        assertEquals(listOf("conv-google-a"), fixture.published.single())
    }

    @Test fun `full run whose claimed recovery is aborted is transient`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001"))).apply {
            recovery = { RcsRecoveryResult.ABORTED }
        }

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run())
    }

    @Test fun `full run cancelled during the last candidate is stopped`() {
        var cancelled = false
        val fixture = Fixture(
            candidates = listOf(candidate("google-a", "+15550000001"), candidate("google-b", "+15550000002")),
        ).apply {
            recovery = { id ->
                if (id == "google-b") cancelled = true
                RcsRecoveryResult.COMPLETE
            }
        }

        assertEquals(RcsBootstrapResult.STOPPED, fixture.bootstrap(current = { !cancelled }).run())
        assertEquals(listOf("google-a", "google-b"), fixture.swept)
    }

    @Test fun `full run with all recoveries complete is complete`() {
        val fixture = Fixture(
            candidates = listOf(candidate("google-a", "+15550000001"), candidate("google-b", "+15550000002")),
        )

        assertEquals(RcsBootstrapResult.COMPLETE, fixture.bootstrap().run())
    }

    @Test fun `ineligible group self and incomplete snapshots are never attempted`() {
        val fixture = Fixture(
            candidates = listOf(
                candidate("group", "+15550000001").put("group", true),
                candidate("ineligible", "+15550000002").put("classification", "unknown"),
                JSONObject().put("google_conversation_id", "two-visible").put("classification", "eligible").put("participants", JSONArray()
                    .put(participant("+15550000003")).put(participant("+15550000004"))),
            ),
        )
        fixture.bootstrap().run()
        assertTrue(fixture.resolved.isEmpty())

        val incomplete = Fixture(candidates = listOf(candidate("google-a", "+15550000001")), complete = false)
        incomplete.bootstrap().run()
        assertEquals(listOf("snapshot"), incomplete.events)
    }

    @Test fun `targeted run passes backfill claims only the target and completes after a complete recovery`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001"), candidate("google-b", "+15550000002")))

        val result = fixture.bootstrap().run(targetGoogleId = "google-b")

        assertEquals(RcsBootstrapResult.COMPLETE, result)
        assertEquals(listOf("google-b"), fixture.resolved)
        assertFalse(JSONObject(fixture.claims.single()).has("backfillFromMs"))
        assertEquals(listOf("google-b"), fixture.swept)
    }

    @Test fun `targeted claim whose recovery is incomplete is transient`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001"))).apply {
            recovery = { RcsRecoveryResult.INCOMPLETE }
        }

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run(targetGoogleId = "google-a"))
    }

    @Test fun `targeted run whose claim is aborted during recovery is transient`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001"))).apply {
            recovery = { RcsRecoveryResult.ABORTED }
        }

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run(targetGoogleId = "google-a"))
    }

    @Test fun `targeted run that cannot resolve or claim is transient`() {
        val fixture = Fixture(
            candidates = listOf(candidate("google-a", "+15550000001")),
            resolution = { NativeRcsBootstrapResolution.Missing },
        )

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run(targetGoogleId = "google-a"))
        assertTrue(fixture.claims.isEmpty())
    }

    @Test fun `targeted target absent from a complete snapshot is transient`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001")))

        assertEquals(RcsBootstrapResult.TRANSIENT, fixture.bootstrap().run(targetGoogleId = "google-z"))
    }

    @Test fun `targeted incomplete snapshot and failed observation are transient`() {
        val incomplete = Fixture(candidates = listOf(candidate("google-a", "+15550000001")), complete = false)
        assertEquals(RcsBootstrapResult.TRANSIENT, incomplete.bootstrap().run(targetGoogleId = "google-a"))

        val observeFails = Fixture(candidates = listOf(candidate("google-a", "+15550000001"))).apply { observeFailure = setOf("google-a") }
        assertEquals(RcsBootstrapResult.TRANSIENT, observeFails.bootstrap().run(targetGoogleId = "google-a"))
    }

    @Test fun `targeted run for an already owned conversation sweeps it instead of claiming`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001")), owned = setOf("google-a"))

        assertEquals(RcsBootstrapResult.COMPLETE, fixture.bootstrap().run(targetGoogleId = "google-a"))

        assertEquals(listOf("google-a"), fixture.swept)
        assertTrue(fixture.resolved.isEmpty())
    }

    @Test fun `a stale lease stops the run before any core or Go work`() {
        val fixture = Fixture(candidates = listOf(candidate("google-a", "+15550000001")))

        assertEquals(RcsBootstrapResult.STOPPED, fixture.bootstrap(current = { false }).run(targetGoogleId = "google-a"))

        assertTrue(fixture.events.isEmpty())
    }

    private class Fixture(
        private val candidates: List<JSONObject>,
        private val owned: Set<String> = emptySet(),
        private val complete: Boolean = true,
        private val resolution: (String) -> NativeRcsBootstrapResolution = { NativeRcsBootstrapResolution.Resolved("conv-$it") },
    ) {
        val events = mutableListOf<String>()
        val resolved = mutableListOf<String>()
        val claims = mutableListOf<String>()
        val published = mutableListOf<List<String>>()
        val swept = mutableListOf<String>()
        var observeFailure = emptySet<String>()
        var claimFailure = emptySet<String>()
        var verifyOutcome: (String) -> NativeRcsMappingVerdict = {
            NativeRcsMappingVerdict(NativeRcsMappingVerification.MAPPED, null)
        }
        var recovery: (String) -> RcsRecoveryResult = { RcsRecoveryResult.COMPLETE }

        fun bootstrap(current: () -> Boolean = { true }) = RcsLinkBootstrap(
            core = object : RcsBootstrapCorePort {
                override fun ownedGoogleIds() = owned.also { events += "owned" }
                override fun resolve(candidates: List<Pair<String, String>>) = candidates.map { (_, googleId) -> resolution(googleId).also {
                    events += "resolve:$googleId"
                    resolved += googleId
                } }
                override fun tuple(snapshot: JSONObject) = RcsNativeContextTuple(1u, 2u, 4u).also { events += "tuple" }
                override fun revision(conversationId: String) = 3uL.also { events += "revision:$conversationId" }
                override fun lifecycleGeneration() = 5uL.also { events += "lifecycle" }
                override fun verify(observation: String, nowMs: Long) = verifyOutcome(observation).also { events += "verify" }
                override fun claim(json: String): NativeRcsLinkOwnershipClaimOutcome {
                    val googleId = JSONObject(json).getString("googleConversationId")
                    events += "claim:$googleId"
                    if (googleId in claimFailure) error("synthetic claim failure")
                    claims += json
                    return NativeRcsLinkOwnershipClaimOutcome.CLAIMED
                }
            },
            link = object : RcsBootstrapLinkPort {
                override fun candidateSnapshot(): String {
                    events += "snapshot"
                    return JSONObject()
                        .put("code", "conversation_candidate_snapshot")
                        .put("complete", complete)
                        .put("pairing_evidence_id", "evidence")
                        .put("conversations", JSONArray(candidates))
                        .toString()
                }

                override fun observe(request: String): String {
                    val googleId = JSONObject(request).getString("google_conversation_id")
                    events += "observe:$googleId"
                    if (googleId in observeFailure) error("synthetic Go failure")
                    return JSONObject()
                        .put("code", "ok")
                        .put("observation", "proof-$googleId")
                        .toString()
                }
            },
            routeReader = { RcsNativeRouteEvidence("route", 1, "+15550009999", RcsNativeOwnNumberSource.SubscriptionManagerPhoneNumber, "US", 800L) },
            clock = object : RcsBootstrapClock { override fun nowMs() = 1_000L },
            publish = { ids -> events += "publish:$ids"; published += ids.toList() },
            sweep = { id -> events += "sweep:$id"; swept += id; recovery(id) },
            current = current,
        )
    }
}

private fun participant(number: String, self: Boolean = false) = JSONObject()
    .put("raw_number", number)
    .put("visible", true)
    .put("self", self)

private fun candidate(googleId: String, number: String) = JSONObject()
    .put("google_conversation_id", googleId)
    .put("classification", "eligible")
    .put("group", false)
    .put("participants", JSONArray().put(participant("+15550009999", self = true)).put(participant(number)))
