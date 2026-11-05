package gmlink

import (
	"encoding/json"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
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

func TestSendRcsRechecksConversationAndSubmitsOneBridgePayload(t *testing.T) {
	auth := linkedSafetyAuth()
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) { knobs.configEmail = fixtureSourceID })
	fixture.mu.Lock()
	fixture.conversations = []*gmproto.Conversation{{
		ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
		Participants: []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	}}
	fixture.mu.Unlock()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	done := startSafetyRun(t, link)
	link.mu.Lock()
	link.m2VerificationPassed = true
	link.mu.Unlock()
	link.projectEvent(&gmproto.Settings{SIMCards: []*gmproto.SIMCard{{SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"}, SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}}, RCSChats: &gmproto.RCSChats{Enabled: true}}}})
	if err := link.RefreshConversationIndex(); err != nil {
		t.Fatal(err)
	}
	result := link.SendRcs(`{"command_id":"synthetic-command","tmp_id":"00000000-0000-4000-8000-000000000000","google_conversation_id":"synthetic-conversation","mapping_revision":1,"proof_digest":"synthetic-proof","content":"synthetic-content"}`)
	if got := decodeSendCode(t, result); got != "submitted" {
		t.Fatalf("code = %s result=%s", got, result)
	}
	fixture.mu.Lock()
	requests := append([]*gmproto.SendMessageRequest(nil), fixture.sendRequests...)
	fixture.mu.Unlock()
	if len(requests) != 1 || requests[0].GetForceRCS() || requests[0].GetTmpID() != "00000000-0000-4000-8000-000000000000" || requests[0].GetMessagePayload().GetTmpID2() != requests[0].GetTmpID() || requests[0].GetMessagePayload().GetParticipantID() != "synthetic-outgoing" {
		t.Fatalf("request = %+v", requests)
	}
	link.Stop()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not stop")
	}
}

func decodeSendCode(t *testing.T, result string) string {
	t.Helper()
	var decoded struct {
		Code string `json:"code"`
	}
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	return decoded.Code
}
