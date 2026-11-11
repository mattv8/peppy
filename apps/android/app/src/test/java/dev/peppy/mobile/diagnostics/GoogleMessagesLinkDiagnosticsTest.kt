package dev.peppy.mobile.diagnostics

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog

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
        "direct_send" to "route_unavailable", "direct_send" to "owner_not_current",
        "direct_send" to "prepare_invalid", "direct_send" to "lease_refused",
        "direct_send" to "target_mapping_refused", "direct_send" to "facts_refused",
        "direct_send" to "route_recheck_refused", "direct_send" to "prepare_failed",
        "direct_send" to "target_binding_failed", "direct_send" to "facts_failed",
        "direct_send" to "group_context_failed", "direct_send" to "lease_failed",
        "direct_send" to "context_failed", "direct_send" to "route_binding_failed",
        "direct_send" to "classification_failed", "direct_send" to "mapping_failed",
        "direct_send" to "route_recheck_failed",
        "group_send" to "group_not_proved", "group_send" to "prepare_invalid",
        "group_send" to "facts_refused", "group_send" to "route_recheck_refused",
        "group_send" to "prepare_failed", "group_send" to "target_binding_failed",
        "group_send" to "facts_failed", "group_send" to "lease_failed",
        "group_send" to "context_failed", "group_send" to "route_binding_failed",
        "group_send" to "classification_failed", "group_send" to "mapping_failed",
        "group_send" to "route_recheck_failed",
        "native_prepare" to "invalid_request", "native_prepare" to "busy",
        "native_prepare" to "link_unavailable", "native_prepare" to "send_unverified",
        "native_prepare" to "run_stale", "native_prepare" to "index_incomplete",
        "native_prepare" to "index_generation", "native_prepare" to "index_epoch",
        "native_prepare" to "pending_events", "native_prepare" to "settings_missing",
        "native_prepare" to "settings_stale", "native_prepare" to "run_evidence_invalid",
        "native_prepare" to "pairing_evidence_invalid", "native_prepare" to "sim_unavailable",
        "native_prepare" to "candidate_too_large", "native_prepare" to "result_too_large",
        "native_prepare" to "prepared",
        "prepare_reply" to "busy", "prepare_reply" to "invalid_request",
        "prepare_reply" to "rejected", "prepare_reply" to "members_changed",
        "prepare_reply" to "prepared", "prepare_reply" to "decode_failed",
        "prepare_reply" to "bridge_failed",
        "readiness" to "requested", "readiness" to "bootstrap_unavailable",
        "readiness" to "refresh_ok", "readiness" to "refresh_pending",
        "readiness" to "refresh_other", "readiness" to "lifecycle_refused",
        "conversation_refresh" to "verification_unavailable", "conversation_refresh" to "runtime_unavailable",
        "conversation_refresh" to "pending_events", "conversation_refresh" to "rpc_inbox",
        "conversation_refresh" to "rpc_archive", "conversation_refresh" to "rpc_spam",
        "conversation_refresh" to "missing_id", "conversation_refresh" to "conflicting_duplicate",
        "conversation_refresh" to "cursor_invalid", "conversation_refresh" to "cursor_repeated",
        "conversation_refresh" to "page_limit", "conversation_refresh" to "item_limit",
        "conversation_refresh" to "publication_stale", "conversation_refresh" to "refreshed",
        "conversation_refresh" to "cancelled",
        "link_lifecycle" to "database_failed", "link_lifecycle" to "keys_unavailable",
        "link_lifecycle" to "invalid_request", "link_lifecycle" to "other_failed",
        "conversation_scan" to "inbox_start", "conversation_scan" to "archive_start", "conversation_scan" to "spam_start",
        "conversation_scan" to "inbox_ok", "conversation_scan" to "archive_ok", "conversation_scan" to "spam_ok",
        "conversation_scan" to "inbox_error", "conversation_scan" to "archive_error", "conversation_scan" to "spam_error",
        "conversation_rpc" to "list_annotation", "conversation_rpc" to "list_normal",
        "conversation_rpc" to "list_http_accepted", "conversation_rpc" to "list_http_failed",
        "conversation_rpc" to "list_unencrypted_ignored", "conversation_rpc" to "list_waiter_matched",
        "conversation_rpc" to "list_waiter_unmatched", "conversation_rpc" to "list_session_missing",
        "conversation_rpc" to "rpc_decrypt_failed",
        "bootstrap" to "snapshot_unavailable", "bootstrap" to "lifecycle_failed", "bootstrap" to "owned_failed",
        "bootstrap" to "resolve_failed", "bootstrap" to "unresolved", "bootstrap" to "tuple_failed",
        "bootstrap" to "revision_failed", "bootstrap" to "route_unavailable", "bootstrap" to "observe_failed",
        "bootstrap" to "verify_failed", "bootstrap" to "not_mapped", "bootstrap" to "claim_failed",
        "bootstrap" to "not_claimed", "session" to "refresh_requested",
        "send_work" to "wake", "send_work" to "busy", "send_work" to "candidates",
        "send_work" to "no_candidates", "send_work" to "pending_without_candidates",
        "send_work" to "lease_unavailable",
        "send_pending" to "attachments", "send_pending" to "not_sms", "send_pending" to "recipients",
        "send_pending" to "single_sms_text",
        "mapping_refusal" to "stale_observation",
        "mapping_refusal" to "google_conversation_ambiguous",
        "mapping_refusal" to "link_unavailable",
        "mapping_refusal" to "ineligible_conversation",
        "mapping_refusal" to "invalid_identity",
        "mapping_refusal" to "participant_ambiguous",
        "mapping_refusal" to "self_chat_mismatch",
        "mapping_refusal" to "peppy_conversation_missing",
        "mapping_refusal" to "peppy_conversation_ambiguous",
        "mapping_refusal" to "mapping_conflict",
        "mapping_refusal" to "nonmonotonic_revision",
        "mapping_refusal" to "retired_context",
        "mapping_refusal" to "peppy_conversation_uncertain",
        "mapping_refusal" to "self_chat_unsupported",
        "mapping_refusal" to "recipient_mismatch",
        "provider_route" to "alert", "provider_route" to "left_to_link", "provider_route" to "pre_proof",
        "provider_route" to "guard_band", "provider_route" to "not_link_owned",
    )

    @Test fun everyPairRecordsOnlyItsStableNumericReason() {
        val records = mutableListOf<String>()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { error("CPU unused") }, records::add) {
            stablePairs.forEach { (kind, code) -> recordGoogleMessagesLinkEvent(kind, code, Long.MAX_VALUE) }
        }
		assertEquals(229, records.size)
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

    @Test fun enabledLogcatMirrorContainsOnlyTheFixedNumericReason() {
        ShadowLog.clear()
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, {}) {
            recordGoogleMessagesLinkEvent("direct_send", "prepare_failed", Long.MAX_VALUE)
            recordGoogleMessagesLinkEvent("private-recipient", "private-error", Long.MIN_VALUE)
        }
        assertEquals(listOf("reason=96", "reason=0"), ShadowLog.getLogsForTag("PeppyRcsDiagnostics").map { it.msg })
    }

    @Test fun disabledDiagnosticsNeverMirrorToLogcat() {
        ShadowLog.clear()
        SyncDiagnostics.withTestConfiguration(false, { error("clock unused") }, { 0L }, {}) {
            recordGoogleMessagesLinkEvent("direct_send", "prepare_failed", 1L)
        }
        assertTrue(ShadowLog.getLogsForTag("PeppyRcsDiagnostics").isEmpty())
    }

    @Test fun throwingSinkCannotEscapeTheRecorder() {
        SyncDiagnostics.withTestConfiguration(true, { 10L }, { 0L }, { error("sink failure") }) {
            recordGoogleMessagesLinkEvent("link", "pair_ok", 1L)
        }
    }
}
