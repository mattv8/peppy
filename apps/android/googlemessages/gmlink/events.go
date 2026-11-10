package gmlink

var eventCodes = []eventCode{
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
