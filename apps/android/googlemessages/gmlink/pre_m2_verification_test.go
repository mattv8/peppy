package gmlink

import (
	"errors"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

// TestM2ConversationIDStableAcrossReconnect catches a pinned schema change that
// removes the common conversation identifier used by list, lookup, and events.
// Server-side stability across a real relink remains owner-gated evidence.
func TestM2ConversationIDStableAcrossReconnect(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-001"}
	list := &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{conversation}}
	get := &gmproto.GetConversationResponse{Conversation: conversation}
	event := &gmproto.UpdateEvents{Event: &gmproto.UpdateEvents_ConversationEvent{
		ConversationEvent: &gmproto.ConversationEvent{Data: []*gmproto.Conversation{conversation}},
	}}

	encoded, err := proto.Marshal(event)
	if err != nil {
		t.Fatal(err)
	}
	var recovered gmproto.UpdateEvents
	if err := proto.Unmarshal(encoded, &recovered); err != nil {
		t.Fatal(err)
	}
	if got, want := list.GetConversations()[0].GetConversationID(), "conversation-001"; got != want {
		t.Fatalf("list conversation id = %q, want %q", got, want)
	}
	if got, want := get.GetConversation().GetConversationID(), "conversation-001"; got != want {
		t.Fatalf("lookup conversation id = %q, want %q", got, want)
	}
	if got, want := recovered.GetConversationEvent().GetData()[0].GetConversationID(), "conversation-001"; got != want {
		t.Fatalf("event conversation id = %q, want %q", got, want)
	}
}

// TestM2TmpIDEchoesInSendAndEvent catches a pinned schema change that prevents
// a submitted request from being correlated with a recovered message event.
func TestM2TmpIDEchoesInSendAndEvent(t *testing.T) {
	const tmpID = "550e8400-e29b-41d4-a716-446655440000"
	request := &gmproto.SendMessageRequest{TmpID: tmpID}
	event := &gmproto.UpdateEvents{Event: &gmproto.UpdateEvents_MessageEvent{
		MessageEvent: &gmproto.MessageEvent{Data: []*gmproto.Message{{TmpID: tmpID}}},
	}}

	encoded, err := proto.Marshal(event)
	if err != nil {
		t.Fatal(err)
	}
	var recovered gmproto.UpdateEvents
	if err := proto.Unmarshal(encoded, &recovered); err != nil {
		t.Fatal(err)
	}
	if got := request.GetTmpID(); got != tmpID {
		t.Fatalf("send request tmp id = %q, want %q", got, tmpID)
	}
	if got := recovered.GetMessageEvent().GetData()[0].GetTmpID(); got != tmpID {
		t.Fatalf("message event tmp id = %q, want %q", got, tmpID)
	}
}

// TestM2ConversationFactsExposeVisibleParticipantsTypeModeWritableAndSim catches
// a pinned schema change that removes a send-time proof input.
func TestM2ConversationFactsExposeVisibleParticipantsTypeModeWritableAndSim(t *testing.T) {
	conversation := &gmproto.Conversation{
		ConversationID:    "conversation-001",
		Type:              gmproto.ConversationType_RCS,
		SendMode:          gmproto.ConversationSendMode_SEND_MODE_AUTO,
		DefaultOutgoingID: "outgoing-001",
		ReadOnly:          false,
		Participants: []*gmproto.Participant{
			{IsMe: true, IsVisible: true},
			{IsMe: false, IsVisible: true},
			{IsMe: false, IsVisible: false},
		},
		SimCard: &gmproto.SIMCard{SIMData: &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 7}}},
	}

	if got := conversation.GetType(); got != gmproto.ConversationType_RCS {
		t.Fatalf("conversation type = %v, want RCS", got)
	}
	if got := conversation.GetSendMode(); got != gmproto.ConversationSendMode_SEND_MODE_AUTO {
		t.Fatalf("send mode = %v, want auto", got)
	}
	if conversation.GetReadOnly() {
		t.Fatal("conversation unexpectedly read only")
	}
	if got, want := conversation.GetDefaultOutgoingID(), "outgoing-001"; got != want {
		t.Fatalf("default outgoing id = %q, want %q", got, want)
	}
	if got, want := conversation.GetSimCard().GetSIMData().GetSIMPayload().GetSIMNumber(), int32(7); got != want {
		t.Fatalf("SIM payload number = %d, want %d", got, want)
	}
	visible, visibleOther := 0, 0
	for _, participant := range conversation.GetParticipants() {
		if !participant.GetIsVisible() {
			continue
		}
		visible++
		if !participant.GetIsMe() {
			visibleOther++
		}
	}
	if visible != 2 || visibleOther != 1 {
		t.Fatalf("visible participants = %d, visible others = %d; want 2 and 1", visible, visibleOther)
	}
}

// TestM2BoundedLookupClassifiesMissAmbiguityAndFailure catches a classification
// regression that could turn an unknown lookup into carrier-only evidence.
func TestM2BoundedLookupClassifiesMissAmbiguityAndFailure(t *testing.T) {
	cases := []struct {
		name  string
		err   error
		count int
		want  string
	}{
		{name: "miss", count: 0, want: "conversation_missing"},
		{name: "ambiguity", count: 2, want: "conversation_ambiguous"},
		{name: "failure", err: errors.New("synthetic lookup failure"), want: "conversation_lookup_failed"},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			if got := classifyM2Lookup(testCase.err, testCase.count); got != testCase.want {
				t.Fatalf("lookup classification = %q, want %q", got, testCase.want)
			}
		})
	}
	status := currentM2VerificationStatus()
	if status.Code != "m2_verification_required" || status.Ready {
		t.Fatalf("verification status = %#v, want fail-closed required", status)
	}
}
