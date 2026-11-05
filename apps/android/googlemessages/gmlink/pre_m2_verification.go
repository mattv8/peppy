package gmlink

// M2VerificationStatus records whether the pinned libgm facts required before
// M2 can use conversation indexing or sending have been established. It holds
// no account, conversation, participant, or message data.
type M2VerificationStatus struct {
	Code                     string
	Ready                    bool
	ConversationIDStable     bool
	TmpIDEcho                bool
	ConversationFactsExposed bool
	LookupFailureClassified  bool
}

// currentM2VerificationStatus fails closed until owner-gated reconnect and
// recovery evidence establishes the two upstream behaviors that static schema
// checks cannot prove.
func currentM2VerificationStatus() M2VerificationStatus {
	status := M2VerificationStatus{
		Code:                     "m2_verification_required",
		ConversationFactsExposed: true,
		LookupFailureClassified:  true,
	}
	status.Ready = status.ConversationIDStable && status.TmpIDEcho && status.ConversationFactsExposed && status.LookupFailureClassified
	return status
}

// classifyM2Lookup keeps a bounded lookup's miss, ambiguity, and transport
// failure distinct so only a confirmed miss can later establish carrier-only.
func classifyM2Lookup(err error, matches int) string {
	switch {
	case err != nil || matches < 0:
		return "conversation_lookup_failed"
	case matches == 0:
		return "conversation_missing"
	case matches == 1:
		return "conversation_found"
	default:
		return "conversation_ambiguous"
	}
}
