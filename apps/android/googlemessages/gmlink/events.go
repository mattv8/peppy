package gmlink

var eventCodes = []eventCode{
	{"conversation_scan", "inbox_start"}, {"conversation_scan", "archive_start"}, {"conversation_scan", "spam_start"},
	{"conversation_scan", "inbox_ok"}, {"conversation_scan", "archive_ok"}, {"conversation_scan", "spam_ok"},
	{"conversation_scan", "inbox_error"}, {"conversation_scan", "archive_error"}, {"conversation_scan", "spam_error"},
	{"conversation_rpc", "list_annotation"}, {"conversation_rpc", "list_normal"},
	{"conversation_rpc", "list_http_accepted"}, {"conversation_rpc", "list_http_failed"},
	{"conversation_rpc", "list_unencrypted_ignored"}, {"conversation_rpc", "list_waiter_matched"},
	{"conversation_rpc", "list_waiter_unmatched"}, {"conversation_rpc", "list_session_missing"},
	{"conversation_rpc", "rpc_decrypt_failed"},
	{"link", "pair_ok"}, {"link", "pair_failed"}, {"link", "pair_no_phone"}, {"link", "cookie_refused"},
	{"link", "pair_multiple_phones"}, {"link", "pair_wrong_emoji"}, {"link", "pair_cancelled"},
	{"link", "pair_timeout"}, {"link", "account_mismatch"}, {"link", "relink_needed"},
	{"link", "relink_ok"}, {"link", "relink_failed"}, {"link", "signed_out"},
	{"link", "unlinked"}, {"link", "unlink_remote_failed"}, {"link", "persist_failed"},
	{"link", "reset_local"}, {"link", "event_overflow"}, {"link", "stale_webview_wiped"}, {"link", "stale_webview_wipe_failed"},
	{"link", "signin_cancelled"}, {"link", "signin_blocked"}, {"link", "signin_incomplete"}, {"link", "pair_declined"},
	{"conn", "ready"}, {"conn", "temporary_error"}, {"conn", "recovered"},
	{"conn", "phone_not_responding"}, {"conn", "phone_responding"}, {"conn", "no_data"},
	{"browser", "active"}, {"browser", "inactive"},
	{"session", "active_session_set"},
	{"health", "ok"}, {"health", "failed"},
	{"conversation", "updated"},
	{"index", "stale"},
	{"native_prepare", "invalid_request"}, {"native_prepare", "busy"}, {"native_prepare", "link_unavailable"},
	{"native_prepare", "send_unverified"}, {"native_prepare", "run_stale"}, {"native_prepare", "index_incomplete"},
	{"native_prepare", "index_generation"}, {"native_prepare", "index_epoch"}, {"native_prepare", "pending_events"},
	{"native_prepare", "settings_missing"}, {"native_prepare", "settings_stale"}, {"native_prepare", "run_evidence_invalid"},
	{"native_prepare", "pairing_evidence_invalid"}, {"native_prepare", "sim_unavailable"}, {"native_prepare", "candidate_too_large"},
	{"native_prepare", "result_too_large"}, {"native_prepare", "prepared"},
	{"conversation_refresh", "verification_unavailable"}, {"conversation_refresh", "runtime_unavailable"}, {"conversation_refresh", "pending_events"},
	{"conversation_refresh", "rpc_inbox"}, {"conversation_refresh", "rpc_archive"}, {"conversation_refresh", "rpc_spam"},
	{"conversation_refresh", "missing_id"}, {"conversation_refresh", "conflicting_duplicate"}, {"conversation_refresh", "cursor_invalid"},
	{"conversation_refresh", "cursor_repeated"}, {"conversation_refresh", "page_limit"}, {"conversation_refresh", "item_limit"},
	{"conversation_refresh", "publication_stale"}, {"conversation_refresh", "refreshed"}, {"conversation_refresh", "cancelled"},
}

type eventCode struct{ kind, code string }

func isEventCode(kind, code string) bool {
	for _, candidate := range eventCodes {
		if candidate.kind == kind && candidate.code == code {
			return true
		}
	}
	return false
}
