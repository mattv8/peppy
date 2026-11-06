package gmlink

import (
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestM6RichProjectionsUseOnlyIdentityAndMetadata(t *testing.T) {
	message := &gmproto.Message{
		MessageID:      "message-opaque",
		ConversationID: "conversation-opaque",
		ReplyMessage:   &gmproto.ReplyMessage{MessageID: "reply-opaque", ConversationID: "conversation-opaque"},
		MessageStatus:  &gmproto.MessageStatus{Status: gmproto.MessageStatusType_OUTGOING_DELIVERED},
		Reactions:      []*gmproto.ReactionEntry{{Data: &gmproto.ReactionData{Unicode: "👍"}, ParticipantIDs: []string{"actor-a", "actor-b"}}},
	}
	reply, reactions, receipt, code := projectM6Rich(message)
	if code != "verified" || reply.ReplyToMessageID != "reply-opaque" || receipt.Status != int32(gmproto.MessageStatusType_OUTGOING_DELIVERED) {
		t.Fatalf("code=%q reply=%#v receipt=%#v", code, reply, receipt)
	}
	if len(reactions) != 1 || reactions[0].ActorCount != 2 || reactions[0].Emoji != "👍" {
		t.Fatalf("reactions=%#v", reactions)
	}
}

func TestM6ReactionFixtureFailsClosedForDeltaSemantics(t *testing.T) {
	if code := m6ReactionDeltaCapability(); code != "reaction_delta_unverified" {
		t.Fatalf("code = %q", code)
	}
}

func TestM6RichProjectionRejectsMissingIdentity(t *testing.T) {
	if _, _, _, code := projectM6Rich(&gmproto.Message{MessageID: "message-only"}); code == "verified" {
		t.Fatal("accepted message without conversation identity")
	}
}
