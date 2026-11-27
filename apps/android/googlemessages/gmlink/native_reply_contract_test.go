package gmlink

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// Cross-language reply fixtures. peppy-mobile-bindings reads the same files in its unit tests.
var replyContractFixtures = filepath.Join("..", "..", "..", "..", "packages", "contracts", "fixtures", "gmlink-replies")

// Rust serialized commit with a reply target. Go must decode it and set the send's Reply.
func TestNativeRcsCommitReplyFixtureDecodesAndSetsReply(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join(replyContractFixtures, "native-commit-reply.json"))
	if err != nil {
		t.Fatal(err)
	}
	commit, ok := decodeNativeRcsCommit(string(raw))
	if !ok {
		t.Fatal("Rust reply commit fixture was rejected")
	}
	if commit.ReplyToGoogleMessageID != "synthetic-reply-target" {
		t.Fatalf("reply id = %q", commit.ReplyToGoogleMessageID)
	}
	googleConversationID := commit.Target.GoogleConversationID
	conversation := &gmproto.Conversation{
		ConversationID: googleConversationID, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "outgoing-1",
		Participants: []*gmproto.Participant{
			{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "outgoing-1"}},
			{IsVisible: true, ID: &gmproto.SmallInfo{Number: "+12025550102"}},
		},
	}
	settings := &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		SIMParticipant: &gmproto.SIMParticipant{ID: "outgoing-1"},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}},
		RCSChats:       &gmproto.RCSChats{Enabled: true},
	}}}
	payload, code := validatedPayload(conversation, settings, rcsSendRequest{
		TmpID: commit.TmpID, GoogleConversationID: googleConversationID, Content: commit.MessageBody,
		ReplyToGoogleMessageID: commit.ReplyToGoogleMessageID,
	}, false)
	if code != "" || payload.GetReply().GetMessageID() != "synthetic-reply-target" {
		t.Fatalf("validatedPayload() = (%v, %q)", payload, code)
	}
}

// Go projected records for a target and a reply to it. peppy-mobile-bindings ingests the same file.
func TestNativeRcsReceiveReplyGolden(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "synthetic-conversation", Type: gmproto.ConversationType_RCS}
	project := func(id, content, replyTo string) rcsProjection {
		message := &gmproto.Message{
			MessageID: id, ConversationID: "synthetic-conversation", TmpID: "synthetic-tmp-" + id,
			Timestamp: 1_700_000_000_000_000, Type: rcsMessageType,
			MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
			MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: content}}}},
		}
		if replyTo != "" {
			message.ReplyMessage = &gmproto.ReplyMessage{MessageID: replyTo}
		}
		return projectRcsMessage(&libgm.WrappedMessage{Message: message}, conversation)
	}
	target := project("synthetic-message", "synthetic target", "")
	reply := project("synthetic-reply", "synthetic reply", "synthetic-message")
	if target.Code != "ok" || target.Record == nil || target.Record.ReplyToGoogleMessageID != "" {
		t.Fatalf("target projection = %#v", target)
	}
	if reply.Code != "ok" || reply.Record == nil || reply.Record.ReplyToGoogleMessageID != "synthetic-message" {
		t.Fatalf("reply projection = %#v", reply)
	}
	tabReply := project("synthetic-tab-reply", "synthetic tab reply", "bad\tid")
	if tabReply.Code != "ok" || tabReply.Record == nil || tabReply.Record.ReplyToGoogleMessageID != "" || !tabReply.InvalidReplyID {
		t.Fatalf("tab reply projection = %#v", tabReply)
	}
	encode := func(projection rcsProjection) json.RawMessage {
		raw, err := json.Marshal(projection)
		if err != nil {
			t.Fatal(err)
		}
		return raw
	}
	assertReplyGolden(t, "receive-reply.json", map[string]json.RawMessage{
		"target":            encode(target),
		"reply":             encode(reply),
		"invalid_tab_reply": encode(tabReply),
	})
}
