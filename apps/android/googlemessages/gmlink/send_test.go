package gmlink

import (
	"encoding/json"
	"testing"
)

func TestSendRcsRequiresVerificationBeforeSubmission(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	var decoded map[string]any
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	if got := decoded["code"]; got != "m2_verification_required" {
		t.Fatalf("code = %v", got)
	}
	if _, exists := decoded["ack_at_ms"]; exists {
		t.Fatalf("verification failure has acknowledgement: %s", result)
	}
}
