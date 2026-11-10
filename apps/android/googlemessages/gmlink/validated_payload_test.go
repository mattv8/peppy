package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestValidatedPayload(t *testing.T) {
	request := rcsSendRequest{TmpID: "00000000-0000-4000-8000-000000000000", GoogleConversationID: "synthetic-conversation", Content: "synthetic-content"}
	validConversation := func() *gmproto.Conversation {
		return &gmproto.Conversation{
			ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
			SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing",
			Participants: []*gmproto.Participant{
				{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-outgoing"}},
				{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}},
			},
		}
	}
	validSettings := func() *gmproto.Settings {
		return &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
			SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"},
			SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}},
			RCSChats:       &gmproto.RCSChats{Enabled: true},
		}}}
	}

	tests := []struct {
		name   string
		change func(*gmproto.Conversation, *gmproto.Settings)
		code   string
	}{
		{"conversation id", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.ConversationID = "other-conversation" }, "conversation_changed"},
		{"non RCS", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.Type = gmproto.ConversationType_SMS }, "conversation_changed"},
		{"inactive", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.Status = gmproto.ConversationStatus_ARCHIVED }, "conversation_changed"},
		{"read only", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.ReadOnly = true }, "conversation_not_writable"},
		{"non automatic send mode", func(c *gmproto.Conversation, _ *gmproto.Settings) {
			c.SendMode = gmproto.ConversationSendMode_SEND_MODE_XMS
		}, "conversation_not_writable"},
		{"no other participant", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.Participants = c.Participants[:1] }, "conversation_not_one_to_one"},
		{"numberless other participant", func(c *gmproto.Conversation, _ *gmproto.Settings) {
			c.Participants = append(c.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-other"}})
		}, "conversation_not_one_to_one"},
		{"missing outgoing id", func(c *gmproto.Conversation, _ *gmproto.Settings) { c.DefaultOutgoingID = "" }, "sim_unavailable"},
		{"RCS disabled", func(_ *gmproto.Conversation, s *gmproto.Settings) { s.SIMCards[0].RCSChats.Enabled = false }, "sim_unavailable"},
		{"missing SIM payload", func(_ *gmproto.Conversation, s *gmproto.Settings) { s.SIMCards[0].SIMData = nil }, "sim_unavailable"},
		{"duplicate SIM", func(_ *gmproto.Conversation, s *gmproto.Settings) { s.SIMCards = append(s.SIMCards, s.SIMCards[0]) }, "sim_unavailable"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			conversation, settings := validConversation(), validSettings()
			test.change(conversation, settings)
			if payload, code := validatedPayload(conversation, settings, request, false); payload != nil || code != test.code {
				t.Fatalf("validatedPayload() = (%v, %q), want (nil, %q)", payload, code, test.code)
			}
		})
	}

	payload, code := validatedPayload(validConversation(), validSettings(), request, false)
	if code != "" || payload == nil || payload.GetConversationID() != request.GoogleConversationID || payload.GetTmpID() != request.TmpID {
		t.Fatalf("validatedPayload() = (%v, %q)", payload, code)
	}
}

func TestValidRcsRequestRequiresCanonicalUUID(t *testing.T) {
	request := rcsSendRequest{CommandID: "synthetic-command", GoogleConversationID: "synthetic-conversation", MappingRevision: 1, ProofDigest: "synthetic-proof", Content: "synthetic-content"}
	for _, tmpID := range []string{
		"00000000-0000-4000-8000-000000000000",
		"{00000000-0000-4000-8000-000000000000}",
		"urn:uuid:00000000-0000-4000-8000-000000000000",
		"00000000000040008000000000000000",
		"00000000-0000-4000-8000-00000000000A",
	} {
		request.TmpID = tmpID
		want := tmpID == "00000000-0000-4000-8000-000000000000"
		if got := validRcsRequest(request); got != want {
			t.Errorf("validRcsRequest(%q) = %t, want %t", tmpID, got, want)
		}
	}
}
