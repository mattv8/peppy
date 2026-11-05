package gmlink

import (
	"encoding/json"

	"github.com/google/uuid"
)

type rcsSendRequest struct {
	CommandID            string `json:"command_id"`
	TmpID                string `json:"tmp_id"`
	GoogleConversationID string `json:"google_conversation_id"`
	MappingRevision      uint64 `json:"mapping_revision"`
	ProofDigest          string `json:"proof_digest"`
	Content              string `json:"content"`
	M2VerificationPassed bool   `json:"m2_verification_passed"`
}

type rcsSendResult struct {
	Code                 string `json:"code"`
	TmpID                string `json:"tmp_id,omitempty"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
	AckAtMS              int64  `json:"ack_at_ms,omitempty"`
}

// SendRcs accepts only an already-permitted command. M2 verification is
// deliberately fail-closed: until the host records both owner-gated procedures,
// this method does not contact libgm. The current pinned API has no reviewed
// settings read RPC, so even a recorded gate cannot submit until that surface is
// independently verified and wired with its own deadline.
func (link *Link) SendRcs(requestJSON string) string {
	var request rcsSendRequest
	if err := json.Unmarshal([]byte(requestJSON), &request); err != nil {
		return sendJSON(rcsSendResult{Code: "invalid_request"})
	}
	if !validRcsRequest(request) {
		return sendJSON(rcsSendResult{Code: "invalid_request"})
	}
	if request.M2VerificationPassed {
		link.mu.Lock()
		if !link.closed {
			link.m2VerificationPassed = true
		}
		link.mu.Unlock()
	}
	if !link.hasM2Verification() {
		return sendJSON(rcsSendResult{Code: "m2_verification_required"})
	}
	return sendJSON(rcsSendResult{Code: "settings_unavailable", TmpID: request.TmpID, GoogleConversationID: request.GoogleConversationID})
}

func validRcsRequest(request rcsSendRequest) bool {
	if request.CommandID == "" || request.GoogleConversationID == "" || request.ProofDigest == "" || request.MappingRevision == 0 || request.Content == "" {
		return false
	}
	parsed, err := uuid.Parse(request.TmpID)
	return err == nil && parsed.Version() == 4
}

func sendJSON(result rcsSendResult) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}
