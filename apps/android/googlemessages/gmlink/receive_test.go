package gmlink

import (
	"encoding/json"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestLibgmReceivePinsStatusAndTombstoneEnums(t *testing.T) {
	statuses := map[gmproto.MessageStatusType]int32{
		gmproto.MessageStatusType_OUTGOING_SENDING:                                      5,
		gmproto.MessageStatusType_OUTGOING_COMPLETE:                                     1,
		gmproto.MessageStatusType_OUTGOING_DELIVERED:                                    2,
		gmproto.MessageStatusType_OUTGOING_DISPLAYED:                                    11,
		gmproto.MessageStatusType_OUTGOING_NOT_DELIVERED_YET:                            14,
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT:                     214,
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_RCS:                      215,
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS:            216,
		gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_TEXT: 236,
	}
	for status, want := range statuses {
		if int32(status) != want {
			t.Fatalf("enum value = %d, want %d", status, want)
		}
	}
	if isTerminalRcsFailure(gmproto.MessageStatusType_OUTGOING_NOT_DELIVERED_YET) {
		t.Fatal("not delivered yet must not be terminal")
	}
	terminalFailures := map[gmproto.MessageStatusType]int32{
		gmproto.MessageStatusType_OUTGOING_FAILED_GENERIC:                                                 8,
		gmproto.MessageStatusType_OUTGOING_FAILED_EMERGENCY_NUMBER:                                        9,
		gmproto.MessageStatusType_OUTGOING_FAILED_TOO_LARGE:                                               13,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_RCS:                                      17,
		gmproto.MessageStatusType_OUTGOING_FAILED_NO_RETRY_NO_FALLBACK:                                    18,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT:                               19,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_ENCRYPTION:                               21,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT_NO_MORE_RETRY:                 22,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_NEGATIVE_DELIVERY:                             24,
		gmproto.MessageStatusType_MESSAGE_STATUS_OUTGOING_FAILED_EMERGENCY_PROTOCOL_DETERMINATION_MESSAGE: 25,
		gmproto.MessageStatusType_OUTGOING_RESTRICTED:                                                     26,
		gmproto.MessageStatusType_OUTGOING_FAILED_TO_ENCRYPT:                                              27,
	}
	for status, want := range terminalFailures {
		if int32(status) != want || !isTerminalRcsFailure(status) {
			t.Fatalf("terminal failure %d is not pinned", want)
		}
	}
}

func TestRcsProjectionProjectsOnlyApprovedTextFields(t *testing.T) {
	message := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID:      "message-1",
		ConversationID: "conversation-1",
		Timestamp:      1234,
		TmpID:          "tmp-1",
		MessageStatus:  &gmproto.MessageStatus{Status: gmproto.MessageStatusType_OUTGOING_DELIVERED},
		MessageInfo: []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{
			MessageContent: &gmproto.MessageContent{Content: "synthetic body"},
		}}},
	}}
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}

	got := projectRcsMessage(message, conversation)
	if got.Code != "ok" {
		t.Fatalf("code = %q, want ok", got.Code)
	}
	if got.Record == nil {
		t.Fatal("record is nil")
	}
	want := rcsReceiveRecord{
		GoogleMessageID:      "message-1",
		GoogleConversationID: "conversation-1",
		Direction:            "outgoing",
		Body:                 "synthetic body",
		SentAtMS:             1234,
		Status:               "delivered",
		TmpID:                "tmp-1",
		RcsReported:          true,
	}
	if *got.Record != want {
		t.Fatalf("record = %#v, want %#v", *got.Record, want)
	}
	encoded, err := json.Marshal(got)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) != `{"code":"ok","record":{"google_message_id":"message-1","google_conversation_id":"conversation-1","direction":"outgoing","body":"synthetic body","sent_at_ms":1234,"status":"delivered","tmp_id":"tmp-1","rcs_reported":true}}` {
		t.Fatalf("JSON = %s", encoded)
	}
}

func TestRcsProjectionRejectsUnsupportedAndUnknownMessages(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	media := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}},
	}}
	if got := projectRcsMessage(media, conversation); got.Code != "unsupported_message" || got.Record != nil {
		t.Fatalf("media projection = %#v", got)
	}

	unknown := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType(999)},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic body"}}}},
	}}
	if got := projectRcsMessage(unknown, conversation); got.Code != "status_unproven" || got.Record != nil {
		t.Fatalf("unknown status projection = %#v", got)
	}
}

func TestRcsProjectionEmitsContentFreeProtocolSwitchSignal(t *testing.T) {
	message := &libgm.WrappedMessage{Message: &gmproto.Message{
		ConversationID: "conversation-1",
		MessageStatus:  &gmproto.MessageStatus{Status: gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS},
	}}

	got := projectRcsMessage(message, nil)
	if got != (rcsProjection{Code: "protocol_switch", GoogleConversationID: "conversation-1"}) {
		t.Fatalf("projection = %#v", got)
	}
}

func TestFetchRcsGapProjectsOnlyMessagesWithOpaquePageToken(t *testing.T) {
	page := &gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{{
			MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234,
			MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
			MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic body"}}}},
		}},
		Cursor: &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234},
	}
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}

	got := projectRcsPage(page, conversation)
	if got.Code != "ok" || got.NextCursor == "" || len(got.Records) != 1 {
		t.Fatalf("page projection = %#v", got)
	}
	if got.Records[0].Cursor != got.NextCursor {
		t.Fatalf("record cursor = %q, next cursor = %q", got.Records[0].Cursor, got.NextCursor)
	}
}
