package dev.peppy.mobile.diagnostics

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GoogleMessagesLinkDiagnosticsTest {
    private val stablePairs = listOf(
        "link" to "pair_ok", "link" to "pair_failed", "link" to "pair_no_phone", "link" to "cookie_refused",
        "link" to "pair_multiple_phones", "link" to "pair_wrong_emoji", "link" to "pair_cancelled",
        "link" to "pair_timeout", "link" to "account_mismatch", "link" to "relink_needed",
        "link" to "relink_ok", "link" to "relink_failed", "link" to "signed_out",
        "link" to "unlinked", "link" to "unlink_remote_failed", "link" to "persist_failed",
        "link" to "reset_local", "link" to "event_overflow", "link" to "stale_webview_wiped",
        "link" to "stale_webview_wipe_failed", "link" to "signin_cancelled", "link" to "signin_blocked",
        "link" to "signin_incomplete", "link" to "pair_declined",
        "conn" to "ready", "conn" to "temporary_error", "conn" to "recovered",
        "conn" to "phone_not_responding", "conn" to "phone_responding", "conn" to "no_data",
        "browser" to "active", "browser" to "inactive", "session" to "active_session_set",
        "health" to "ok", "health" to "failed", "conversation" to "updated", "link" to "rcs_cutover_ambiguous",
        "link" to "rcs_eligibility_failed", "link" to "rcs_cutover_sweep_failed", "link" to "rcs_receive_sweep_failed", "link" to "rcs_callback_failed", "link" to "rcs_bootstrap_failed", "link" to "explicit_unlink_completion_failed",
        "link" to "rcs_recovery_aborted", "link" to "rcs_recovery_incomplete", "link" to "rcs_drain_evicted", "link" to "rcs_sweep_skipped_idle",
			"index" to "stale",
            "live" to "auth_refused", "live" to "protocol_fatal", "live" to "reconnect_scheduled",
            "live" to "enabled", "live" to "disabled",
            "typing" to "enabled", "typing" to "disabled", "typing" to "inbound_sealed",
            "typing" to "outbound_effect", "typing" to "suppressed", "typing" to "dropped_stale",
        "create" to "begin_failed", "create" to "commit_failed", "create" to "link_unavailable",
        "create" to "unknown", "create" to "not_rcs",
        "reconcile" to "snapshot_unavailable", "reconcile" to "ingest_failed", "reconcile" to "after_create_failed",
        "group_send" to "submitted", "group_send" to "members_changed", "group_send" to "outcome_unknown",
        "group_send" to "refused", "group_send" to "route_not_current", "group_send" to "target_refused",
        "group_send" to "proof_refused", "group_send" to "begin_refused", "group_send" to "commit_rejected",
        "group_send" to "lease_refused", "create" to "settle_failed",
        "direct_send" to "submitted", "direct_send" to "carrier_only", "direct_send" to "route_not_current",
        "direct_send" to "target_refused", "direct_send" to "proof_refused", "direct_send" to "begin_refused",
        "direct_send" to "commit_rejected", "direct_send" to "outcome_unknown", "direct_send" to "refused",
        "direct_send" to "proof_window_missed",
    )

    @Test fun everyPairRecordsOnlyItsStableNumericReason() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { error("CPU unused") }, records::add) {
            stablePairs.forEach { (kind, code) -> recordGoogleMessagesLinkEvent(kind, code, Long.MAX_VALUE) }
        }
		assertEquals(88, records.size)
        records.forEachIndexed { index, record ->
            assertEquals("event=27 session=1 timestamp_ms=10 trace=0 29=${index + 1}", record)
        }
        assertEquals(27L, SyncTraceEvent.GM_LINK.code)
        assertEquals(SyncTraceEvent.entries.size, SyncTraceEvent.entries.map { it.code }.toSet().size)
    }

    @Test fun goEventVocabularyIsCoveredByTheClosedMapping() {
        val root = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .first { File(it, "apps/android/googlemessages/gmlink/events.go").isFile }
        val goSource = File(root, "apps/android/googlemessages/gmlink/events.go").readText()
        val goPairs = Regex("""\{"([^"]+)",\s*"([^"]+)"}""").findAll(goSource)
            .map { it.groupValues[1] to it.groupValues[2] }.toSet()
        assertTrue(goPairs.isNotEmpty())
        assertTrue(GoogleMessagesLinkDiagnosticReason.codes.keys.containsAll(goPairs))
    }

    @Test fun unknownMalformedAndMismatchedPairsRecordOnlyUnknown() {
        val records = mutableListOf<String>()
        val unknownPairs = listOf(
            "link" to "unknown", "link" to "new_code", "conn" to "pair_ok", "unknown" to "pair_ok",
            "link" to "", "link" to "bad-code", "link" to "pair_ok\n", "link" to "UPPER",
            "link" to "a".repeat(41), "link\n" to "pair_ok", "stop_code" to "bad code",
        )
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, records::add) {
            unknownPairs.forEach { (kind, code) -> recordGoogleMessagesLinkEvent(kind, code, Long.MIN_VALUE) }
        }
        assertEquals(List(unknownPairs.size) { "event=27 session=1 timestamp_ms=10 trace=0 29=0" }, records)
    }

    @Test fun disabledRecordingDoesNotReadClockOrCallSink() {
        SyncDiagnostics.withTestConfiguration(false, { error("clock unused") }, { error("CPU unused") }, { error("sink unused") }) {
            recordGoogleMessagesLinkEvent("link", "pair_ok", 1L)
        }
    }

    @Test fun throwingSinkCannotEscapeTheRecorder() {
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, { error("sink failure") }) {
            recordGoogleMessagesLinkEvent("link", "pair_ok", 1L)
        }
    }
}
