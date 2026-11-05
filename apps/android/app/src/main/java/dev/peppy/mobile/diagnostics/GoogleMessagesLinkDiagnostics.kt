package dev.peppy.mobile.diagnostics

/** Append-only reason numbers shared by Go callbacks and native link events. */
internal object GoogleMessagesLinkDiagnosticReason {
    const val UNKNOWN = 0L

    val codes: Map<Pair<String, String>, Long> = mapOf(
        ("link" to "pair_ok") to 1L,
        ("link" to "pair_failed") to 2L,
        ("link" to "pair_no_phone") to 3L,
        ("link" to "cookie_refused") to 4L,
        ("link" to "pair_multiple_phones") to 5L,
        ("link" to "pair_wrong_emoji") to 6L,
        ("link" to "pair_cancelled") to 7L,
        ("link" to "pair_timeout") to 8L,
        ("link" to "account_mismatch") to 9L,
        ("link" to "relink_needed") to 10L,
        ("link" to "relink_ok") to 11L,
        ("link" to "relink_failed") to 12L,
        ("link" to "signed_out") to 13L,
        ("link" to "unlinked") to 14L,
        ("link" to "unlink_remote_failed") to 15L,
        ("link" to "persist_failed") to 16L,
        ("link" to "reset_local") to 17L,
        ("link" to "event_overflow") to 18L,
        ("link" to "stale_webview_wiped") to 19L,
        ("link" to "stale_webview_wipe_failed") to 20L,
        ("link" to "signin_cancelled") to 21L,
        ("link" to "signin_blocked") to 22L,
        ("link" to "signin_incomplete") to 23L,
        ("link" to "pair_declined") to 24L,
        ("conn" to "ready") to 25L,
        ("conn" to "temporary_error") to 26L,
        ("conn" to "recovered") to 27L,
        ("conn" to "phone_not_responding") to 28L,
        ("conn" to "phone_responding") to 29L,
        ("conn" to "no_data") to 30L,
        ("browser" to "active") to 31L,
        ("browser" to "inactive") to 32L,
        ("session" to "active_session_set") to 33L,
        ("health" to "ok") to 34L,
        ("health" to "failed") to 35L,
        ("conversation" to "updated") to 36L,
    )
}

/** Raw callback values are deliberately discarded, including health timings. */
@Suppress("UNUSED_PARAMETER")
internal fun recordGoogleMessagesLinkEvent(kind: String, code: String, value: Long) {
    if (!SyncDiagnostics.enabled) return
    // Exact pair lookup rejects unknown and malformed strings without recording either input.
    val reason = GoogleMessagesLinkDiagnosticReason.codes[kind to code] ?: GoogleMessagesLinkDiagnosticReason.UNKNOWN
    SyncDiagnostics.record(SyncTraceEvent.GM_LINK, SyncTraceField.REASON to reason)
}
