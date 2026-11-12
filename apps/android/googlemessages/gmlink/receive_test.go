package gmlink

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

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
		Timestamp:      1735689600123456,
		Type:           rcsMessageType,
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
		SentAtMS:             1735689600123,
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
	if string(encoded) != `{"code":"ok","record":{"google_message_id":"message-1","google_conversation_id":"conversation-1","direction":"outgoing","body":"synthetic body","sent_at_ms":1735689600123,"status":"delivered","tmp_id":"tmp-1","rcs_reported":true}}` {
		t.Fatalf("JSON = %s", encoded)
	}
}

func TestRcsProjectionReportsTheOtherIDsOfAMessage(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	message := func(another *gmproto.AnotherMessageID) *libgm.WrappedMessage {
		return &libgm.WrappedMessage{Message: &gmproto.Message{
			MessageID:        "message-1",
			ConversationID:   "conversation-1",
			Type:             rcsMessageType,
			MessageStatus:    &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
			AnotherMessageID: another,
			MessageInfo: []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{
				MessageContent: &gmproto.MessageContent{Content: "synthetic body"},
			}}},
		}}
	}
	for name, test := range map[string]struct {
		another *gmproto.AnotherMessageID
		want    string
	}{
		"none":            {nil, ""},
		"same as primary": {&gmproto.AnotherMessageID{MessageID: "message-1"}, ""},
		"both":            {&gmproto.AnotherMessageID{MessageID: "legacy-1", AnotherID: "MxOther"}, `"alternate_message_ids":["legacy-1","MxOther"]`},
		"repeated":        {&gmproto.AnotherMessageID{MessageID: "MxOther", AnotherID: "MxOther"}, `"alternate_message_ids":["MxOther"]`},
		"oversized":       {&gmproto.AnotherMessageID{AnotherID: strings.Repeat("x", 513)}, ""},
	} {
		got := projectRcsMessage(message(test.another), conversation)
		if got.Code != "ok" {
			t.Fatalf("%s: code = %q, want ok", name, got.Code)
		}
		encoded, err := json.Marshal(got.Record)
		if err != nil {
			t.Fatal(err)
		}
		if test.want == "" && strings.Contains(string(encoded), "alternate_message_ids") {
			t.Fatalf("%s: JSON = %s, want no alternate ids", name, encoded)
		}
		if !strings.Contains(string(encoded), test.want) {
			t.Fatalf("%s: JSON = %s, want %s", name, encoded, test.want)
		}
	}
}

func TestRcsProjectionRejectsUnsupportedAndUnknownMessages(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	media := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}},
	}}
	if got := projectRcsMessage(media, conversation); got.Code != "unsupported_message" || got.Record != nil {
		t.Fatalf("media projection = %#v", got)
	} else {
		assertRejectedProjectionHasNoBody(t, got)
	}

	unknown := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType(999)},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic body"}}}},
	}}
	if got := projectRcsMessage(unknown, conversation); got.Code != "status_unproven" || got.Record != nil {
		t.Fatalf("unknown status projection = %#v", got)
	} else {
		assertRejectedProjectionHasNoBody(t, got)
	}
	missingMessageID := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE)
	missingMessageID.MessageID = ""
	if got := projectRcsMessage(missingMessageID, conversation); got.Code != "identity_missing" || got.Record != nil {
		t.Fatalf("missing message id projection = %#v", got)
	}
	missingConversationID := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE)
	missingConversationID.ConversationID = ""
	if got := projectRcsMessage(missingConversationID, conversation); got.Code != "identity_missing" || got.Record != nil {
		t.Fatalf("missing conversation id projection = %#v", got)
	}
}

func TestRcsProjectionRequiresMessageLevelRcsEvidence(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	for _, test := range []struct {
		name     string
		message  int64
		accepted bool
	}{
		{name: "rcs", message: 4, accepted: true},
		{name: "sms", message: 1},
		{name: "downloaded mms", message: 2},
		{name: "undownloaded mms", message: 3},
		{name: "unknown", message: 99},
	} {
		t.Run(test.name, func(t *testing.T) {
			message := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE)
			message.Type = test.message
			got := projectRcsMessage(message, conversation)
			if test.accepted {
				if got.Code != "ok" || got.Record == nil {
					t.Fatalf("projection = %#v", got)
				}
				return
			}
			if got.Code != "message_unproven" || got.Record != nil {
				t.Fatalf("projection = %#v", got)
			}
		})
	}
}

func TestRcsProjectionKeepsIncomingStatusesIncoming(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	for _, status := range []gmproto.MessageStatusType{
		gmproto.MessageStatusType_INCOMING_COMPLETE,
		gmproto.MessageStatusType_INCOMING_DELIVERED,
		gmproto.MessageStatusType_INCOMING_DISPLAYED,
	} {
		t.Run(status.String(), func(t *testing.T) {
			got := projectRcsMessage(textMessage(status), conversation)
			if got.Code != "ok" || got.Record == nil || got.Record.Direction != "incoming" || got.Record.Status != "sent" {
				t.Fatalf("projection = %#v", got)
			}
		})
	}
}

func TestRcsProjectionMapsPinnedStatuses(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	cases := []struct {
		name      string
		status    gmproto.MessageStatusType
		direction string
		want      string
		accepted  bool
	}{
		{"incoming", gmproto.MessageStatusType_INCOMING_COMPLETE, "incoming", "sent", true},
		{"sending", gmproto.MessageStatusType_OUTGOING_SENDING, "outgoing", "sending", true},
		{"sent", gmproto.MessageStatusType_OUTGOING_COMPLETE, "outgoing", "sent", true},
		{"delivered", gmproto.MessageStatusType_OUTGOING_DELIVERED, "outgoing", "delivered", true},
		{"read", gmproto.MessageStatusType_OUTGOING_DISPLAYED, "outgoing", "read", true},
		{"failure 8", gmproto.MessageStatusType_OUTGOING_FAILED_GENERIC, "outgoing", "failed", true},
		{"failure 9", gmproto.MessageStatusType_OUTGOING_FAILED_EMERGENCY_NUMBER, "outgoing", "failed", true},
		{"failure 13", gmproto.MessageStatusType_OUTGOING_FAILED_TOO_LARGE, "outgoing", "failed", true},
		{"failure 17", gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_RCS, "outgoing", "failed", true},
		{"failure 18", gmproto.MessageStatusType_OUTGOING_FAILED_NO_RETRY_NO_FALLBACK, "outgoing", "failed", true},
		{"failure 19", gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT, "outgoing", "failed", true},
		{"failure 21", gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_ENCRYPTION, "outgoing", "failed", true},
		{"failure 22", gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT_NO_MORE_RETRY, "outgoing", "failed", true},
		{"failure 24", gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_NEGATIVE_DELIVERY, "outgoing", "failed", true},
		{"failure 25", gmproto.MessageStatusType_MESSAGE_STATUS_OUTGOING_FAILED_EMERGENCY_PROTOCOL_DETERMINATION_MESSAGE, "outgoing", "failed", true},
		{"failure 26", gmproto.MessageStatusType_OUTGOING_RESTRICTED, "outgoing", "failed", true},
		{"failure 27", gmproto.MessageStatusType_OUTGOING_FAILED_TO_ENCRYPT, "outgoing", "failed", true},
		{"not delivered yet", gmproto.MessageStatusType_OUTGOING_NOT_DELIVERED_YET, "", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := projectRcsMessage(textMessage(tc.status), conversation)
			if !tc.accepted {
				if got.Code != "status_unproven" || got.Record != nil {
					t.Fatalf("projection = %#v", got)
				}
				return
			}
			if got.Code != "ok" || got.Record == nil || got.Record.Direction != tc.direction || got.Record.Status != tc.want {
				t.Fatalf("projection = %#v", got)
			}
		})
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
			MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234, Type: rcsMessageType,
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
	encoded, err := json.Marshal(got)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), `"cursor"`) {
		t.Fatalf("record cursor present in %s", encoded)
	}
	if !got.HasMore {
		t.Fatalf("continuing page = %#v", got)
	}

	for _, page := range []*gmproto.ListMessagesResponse{
		{},
		{Cursor: &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234}},
	} {
		exhausted := projectRcsPage(page, conversation)
		if exhausted.Code != "ok" || exhausted.HasMore || len(exhausted.Records) != 0 || exhausted.NextCursor != "" {
			t.Fatalf("exhausted page = %#v", exhausted)
		}
	}
	withoutResponseCursor := projectRcsPage(&gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message},
	}, conversation)
	if withoutResponseCursor.Code != "ok" || !withoutResponseCursor.HasMore || withoutResponseCursor.NextCursor != "AAABlB8pfHttZXNzYWdlLTE" || len(withoutResponseCursor.Records) != 1 {
		t.Fatalf("cursor-less page = %#v", withoutResponseCursor)
	}
	malformed := projectRcsPage(&gmproto.ListMessagesResponse{Cursor: &gmproto.Cursor{}}, conversation)
	if malformed.Code != "ok" || malformed.HasMore || len(malformed.Records) != 0 || malformed.NextCursor != "" {
		t.Fatalf("empty malformed-cursor page = %#v", malformed)
	}
}

func TestFetchRcsGapKeepsProtocolSwitchSignalsAndText(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	cases := []struct {
		name   string
		status gmproto.MessageStatusType
	}{
		{"text", gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT},
		{"rcs", gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_RCS},
		{"encrypted rcs", gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS},
		{"encrypted rcs info", gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS_INFO},
		{"group rcs to encrypted", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_GROUP_PROTOCOL_SWITCH_RCS_TO_E2EE},
		{"group encrypted to rcs", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_GROUP_PROTOCOL_SWITCH_E2EE_TO_RCS},
		{"text to encrypted", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_TEXT_TO_E2EE},
		{"encrypted text", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_TEXT},
		{"rcs to encrypted", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_RCS_TO_E2EE},
		{"encrypted to rcs", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_RCS},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			tombstone := textMessage(tc.status)
			got := projectRcsPage(&gmproto.ListMessagesResponse{
				Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message, tombstone.Message},
				Cursor:   &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234},
			}, conversation)
			if got.Code != "ok" || !got.HasMore || got.NextCursor == "" || len(got.Records) != 1 {
				t.Fatalf("page = %#v", got)
			}
			if len(got.Signals) != 1 || got.Signals[0] != (rcsPageSignal{Code: "protocol_switch", GoogleConversationID: "conversation-1"}) {
				t.Fatalf("signals = %#v", got.Signals)
			}
		})
	}
}

func TestRcsPageContinuesPastKnownInformationalTombstones(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	first := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message
	first.MessageID = "first"
	tombstone := &gmproto.Message{ConversationID: "conversation-1", MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_TOMBSTONE_PARTICIPANT_JOINED}}
	last := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message
	last.MessageID = "last"
	got := projectRcsPage(&gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{first, tombstone, last},
		Cursor:   &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234},
	}, conversation)
	if got.Code != "ok" || len(got.Records) != 2 || got.Records[0].GoogleMessageID != "first" || got.Records[1].GoogleMessageID != "last" || got.NextCursor != opaquePageToken(&gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234}) {
		t.Fatalf("page = %#v", got)
	}
}

func TestRcsPageAdvancesCursorForKnownInformationalTombstones(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	got := projectRcsPage(&gmproto.ListMessagesResponse{
		Messages: []*gmproto.Message{{ConversationID: "conversation-1", MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_TOMBSTONE_PARTICIPANT_LEFT}}},
		Cursor:   &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234},
	}, conversation)
	if got.Code != "ok" || len(got.Records) != 0 || len(got.Signals) != 0 || !got.HasMore || got.NextCursor != opaquePageToken(&gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234}) {
		t.Fatalf("page = %#v", got)
	}
}

func protocolSwitchMessage(status gmproto.MessageStatusType) *libgm.WrappedMessage {
	return &libgm.WrappedMessage{Message: &gmproto.Message{
		ConversationID: "conversation-1",
		MessageStatus:  &gmproto.MessageStatus{Status: status},
	}}
}

func textMessage(status gmproto.MessageStatusType) *libgm.WrappedMessage {
	return &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1735689600123456, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: status},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MessageContent{MessageContent: &gmproto.MessageContent{Content: "synthetic body"}}}},
	}}
}

func assertRejectedProjectionHasNoBody(t *testing.T, projection rcsProjection) {
	t.Helper()
	encoded, err := json.Marshal(projection)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) == "" || string(encoded) == `{"code":"ok"}` || string(encoded) == `{"body":"synthetic body"}` {
		t.Fatalf("rejected projection = %s", encoded)
	}
	if string(encoded) != `{"code":"unsupported_message"}` && string(encoded) != `{"code":"status_unproven"}` {
		t.Fatalf("rejected projection contains unapproved fields: %s", encoded)
	}
}

type ownedRecordHost struct {
	rcsRecords         []string
	rcsStreamEpochs    []int64
	typingRecords      []string
	typingStreamEpochs []int64
}

func (*ownedRecordHost) PersistState([]byte) error                 { return nil }
func (*ownedRecordHost) Event(string, string, int64, int64, int64) {}
func (*ownedRecordHost) ShowEmoji(string, int64, int64)            {}
func (host *ownedRecordHost) RcsEvent(record string, _, _, streamEpoch int64) {
	host.rcsRecords = append(host.rcsRecords, record)
	host.rcsStreamEpochs = append(host.rcsStreamEpochs, streamEpoch)
}
func (host *ownedRecordHost) TypingEvent(record string, _, _, streamEpoch int64) {
	host.typingRecords = append(host.typingRecords, record)
	host.typingStreamEpochs = append(host.typingStreamEpochs, streamEpoch)
}

func receiveWiringLink(host Host) *Link {
	conversation := typingConversation("conversation-1", true)
	return &Link{
		host:          host,
		generation:    7,
		runGeneration: 7,
		streamEpoch:   3,
		stateRevision: 11,
		ownerVerification: &ownerVerification{generation: 7, Capabilities: []string{
			verificationCapabilityReceive,
			verificationCapabilityIndex,
			verificationCapabilitySend,
			verificationCapabilityTyping,
			verificationCapabilityGroups,
		}},
		changes:                      make(chan any, 8),
		conversationIndexComplete:    true,
		conversationIndexGeneration:  7,
		conversationIndexStreamEpoch: 3,
		conversationRecords:          map[string]*gmproto.Conversation{"conversation-1": conversation},
	}
}

func drainQueuedEvents(link *Link) {
	for len(link.changes) > 0 {
		link.consumeQueuedEvent(<-link.changes)
	}
}

func TestLinkProjectsLiveMessageAndStatusAsOwnedRecords(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	for _, status := range []gmproto.MessageStatusType{
		gmproto.MessageStatusType_INCOMING_COMPLETE,
		gmproto.MessageStatusType_OUTGOING_DELIVERED,
	} {
		message := textMessage(status)
		link.queueEventFor(7, message)
		drainQueuedEvents(link)
	}
	if len(host.rcsRecords) != 2 {
		t.Fatalf("records = %v", host.rcsRecords)
	}
	var incoming, delivered rcsProjection
	if err := json.Unmarshal([]byte(host.rcsRecords[0]), &incoming); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(host.rcsRecords[1]), &delivered); err != nil {
		t.Fatal(err)
	}
	if incoming.Code != "ok" || incoming.Record == nil || incoming.Record.Direction != "incoming" || incoming.Record.Body != "synthetic body" {
		t.Fatalf("incoming = %+v", incoming)
	}
	if delivered.Code != "ok" || delivered.Record == nil || delivered.Record.Status != "delivered" {
		t.Fatalf("delivered = %+v", delivered)
	}
}

func TestLinkProjectsUnsupportedAndTombstoneWithoutContent(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	media := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "media", ConversationID: "conversation-1", Timestamp: 1, Type: rcsMessageType,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}},
	}}
	tombstone := textMessage(gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT)
	for _, event := range []*libgm.WrappedMessage{media, tombstone} {
		link.queueEventFor(7, event)
		drainQueuedEvents(link)
	}
	if len(host.rcsRecords) != 2 || host.rcsRecords[0] != `{"code":"unsupported_message"}` || host.rcsRecords[1] != `{"code":"protocol_switch","google_conversation_id":"conversation-1"}` {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestLinkDropsKnownInformationalTombstones(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	link.queueEventFor(7, protocolSwitchMessage(gmproto.MessageStatusType_TOMBSTONE_PARTICIPANT_JOINED))
	drainQueuedEvents(link)
	if len(host.rcsRecords) != 0 {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestCarrierTransitionTombstonesRemainLiveAndHistoryControlSignals(t *testing.T) {
	transitions := []struct {
		name   string
		status gmproto.MessageStatusType
		value  int32
	}{
		{name: "switch to group mms", status: gmproto.MessageStatusType_TOMBSTONE_SWITCH_TO_GROUP_MMS, value: 208},
		{name: "switch to broadcast sms", status: gmproto.MessageStatusType_TOMBSTONE_SWITCH_TO_BROADCAST_SMS, value: 209},
		{name: "group encrypted to mms", status: gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_GROUP_PROTOCOL_SWITCH_E2EE_TO_MMS, value: 252},
	}
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	for _, transition := range transitions {
		t.Run(transition.name, func(t *testing.T) {
			if int32(transition.status) != transition.value {
				t.Fatalf("enum value = %d, want %d", transition.status, transition.value)
			}
			t.Run("live ingress", func(t *testing.T) {
				host := &ownedRecordHost{}
				link := receiveWiringLink(host)
				link.queueEventFor(7, textMessage(transition.status))
				drainQueuedEvents(link)
				if len(host.rcsRecords) != 1 {
					t.Fatalf("live control records = %v, want exactly one", host.rcsRecords)
				}
				if got := host.rcsRecords[0]; got != `{"code":"protocol_switch","google_conversation_id":"conversation-1"}` {
					t.Fatalf("live record = %s", got)
				}
			})

			t.Run("history", func(t *testing.T) {
				page := projectRcsPage(&gmproto.ListMessagesResponse{
					Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message, textMessage(transition.status).Message},
					Cursor:   &gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234},
				}, conversation)
				if page.Code != "ok" || len(page.Records) != 1 || len(page.Signals) != 1 || page.Signals[0] != (rcsPageSignal{Code: "protocol_switch", GoogleConversationID: "conversation-1"}) {
					t.Fatalf("history page = %#v", page)
				}
				if !page.HasMore || page.NextCursor != opaquePageToken(&gmproto.Cursor{LastItemID: "page-id", LastItemTimestamp: 1234}) {
					t.Fatalf("history cursor = %#v", page)
				}
			})
		})
	}
}

func TestLinkProjectsProtocolSwitchAfterConversationBecomesSMS(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	link.conversationRecords["conversation-1"].Type = gmproto.ConversationType_SMS

	link.queueEventFor(7, protocolSwitchMessage(gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT))
	drainQueuedEvents(link)

	if len(host.rcsRecords) != 1 || host.rcsRecords[0] != `{"code":"protocol_switch","google_conversation_id":"conversation-1"}` {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestLinkProjectsE2EEProtocolSwitchWithoutCompleteIndex(t *testing.T) {
	for _, status := range []gmproto.MessageStatusType{
		gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_ENCRYPTED_RCS,
		gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_TEXT,
	} {
		t.Run(status.String(), func(t *testing.T) {
			host := &ownedRecordHost{}
			link := receiveWiringLink(host)
			link.conversationIndexComplete = false

			link.queueEventFor(7, protocolSwitchMessage(status))
			drainQueuedEvents(link)

			if len(host.rcsRecords) != 1 || host.rcsRecords[0] != `{"code":"protocol_switch","google_conversation_id":"conversation-1"}` {
				t.Fatalf("records = %v", host.rcsRecords)
			}
		})
	}
}

func TestLinkDropsStaleStreamSignalsAndBodies(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	stale := queuedEvent{generation: 7, streamEpoch: 2}

	link.projectRcsEventFor(stale, protocolSwitchMessage(gmproto.MessageStatusType_TOMBSTONE_PROTOCOL_SWITCH_TO_TEXT))
	link.projectRcsEventFor(stale, textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE))

	if len(host.rcsRecords) != 0 {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestLinkDoesNotLeakUnsupportedBody(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	link.conversationRecords["conversation-1"].Type = gmproto.ConversationType_SMS

	link.queueEventFor(7, textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE))
	drainQueuedEvents(link)

	if len(host.rcsRecords) != 1 || host.rcsRecords[0] != `{"code":"conversation_unsupported"}` || strings.Contains(host.rcsRecords[0], "synthetic body") {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestLinkDropsOldReceiveGenerationAndPreservesOrder(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	link.queueEventFor(6, textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE))
	for index := 1; index <= 2; index++ {
		message := textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE)
		message.Message.MessageID = fmt.Sprintf("message-%d", index)
		link.queueEventFor(7, message)
	}
	drainQueuedEvents(link)
	if len(host.rcsRecords) != 2 {
		t.Fatalf("records = %v", host.rcsRecords)
	}
	for index, encoded := range host.rcsRecords {
		var projection rcsProjection
		if err := json.Unmarshal([]byte(encoded), &projection); err != nil {
			t.Fatal(err)
		}
		want := fmt.Sprintf("message-%d", index+1)
		if projection.Record == nil || projection.Record.GoogleMessageID != want {
			t.Fatalf("record %d = %+v", index, projection.Record)
		}
	}
}

func fetchGapTestLink(fetch func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error)) *Link {
	return &Link{
		client:        &libgm.Client{},
		state:         stateLinked,
		running:       true,
		runCtx:        context.Background(),
		generation:    7,
		runGeneration: 7,
		streamEpoch:   3,
		ownerVerification: &ownerVerification{generation: 7, Capabilities: []string{
			verificationCapabilityReceive,
			verificationCapabilityIndex,
			verificationCapabilitySend,
			verificationCapabilityTyping,
			verificationCapabilityGroups,
		}},
		conversationRecords: map[string]*gmproto.Conversation{
			"conversation-1": typingConversation("conversation-1", true),
		},
		conversationIndexComplete:    true,
		conversationIndexGeneration:  7,
		conversationIndexStreamEpoch: 3,
		fetchMessages:                fetch,
	}
}

func decodeGapResult(t *testing.T, encoded string) rcsPageProjection {
	t.Helper()
	var result rcsPageProjection
	if err := json.Unmarshal([]byte(encoded), &result); err != nil {
		t.Fatalf("decode result: %v (%q)", err, encoded)
	}
	return result
}

func TestFetchRcsGapReturnsEmptyPage(t *testing.T) {
	calls := 0
	link := fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		calls++
		return &gmproto.ListMessagesResponse{}, nil
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

	if got.Code != "ok" || got.HasMore || len(got.Records) != 0 || len(got.Signals) != 0 || calls != 1 {
		t.Fatalf("result = %#v, calls = %d", got, calls)
	}
}

func TestFetchRcsGapContinuesFromOpaquePageToken(t *testing.T) {
	start := &gmproto.Cursor{LastItemID: "start", LastItemTimestamp: 1234}
	next := &gmproto.Cursor{LastItemID: "next", LastItemTimestamp: 5678}
	link := fetchGapTestLink(func(_ context.Context, conversationID string, count int64, cursor *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		if conversationID != "conversation-1" || count != rcsGapPageSize || cursor.GetLastItemID() != "start" || cursor.GetLastItemTimestamp() != 1234 {
			t.Fatalf("fetch arguments = %q, %d, %#v", conversationID, count, cursor)
		}
		return &gmproto.ListMessagesResponse{Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message}, Cursor: next}, nil
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", opaquePageToken(start)))

	if got.Code != "ok" || !got.HasMore || got.NextCursor != opaquePageToken(next) || len(got.Records) != 1 {
		t.Fatalf("result = %#v", got)
	}
}

func TestFetchRcsGapRejectsMalformedPageToken(t *testing.T) {
	called := false
	link := fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		called = true
		return nil, nil
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", "not-a-token"))

	if got.Code != "invalid_page_token" || called {
		t.Fatalf("result = %#v, called = %t", got, called)
	}
}

func TestFetchRcsGapHonorsCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	link := fetchGapTestLink(func(ctx context.Context, _ string, _ int64, _ *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		return nil, ctx.Err()
	})
	link.runCtx = ctx

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

	if got.Code != "gap_cancelled" || got.HasMore || got.NextCursor != "" || len(got.Records) != 0 {
		t.Fatalf("result = %#v", got)
	}
}

func TestFetchRcsGapCancellationDoesNotWaitForRPCMutex(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	called := false
	link := fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		called = true
		return nil, nil
	})
	link.runCtx = ctx
	link.rpcMu.Lock()
	defer link.rpcMu.Unlock()
	result := make(chan string, 1)
	go func() { result <- link.FetchRcsGap("conversation-1", "") }()
	select {
	case encoded := <-result:
		got := decodeGapResult(t, encoded)
		if got.Code != "gap_cancelled" || called {
			t.Fatalf("result = %#v, called = %t", got, called)
		}
	case <-time.After(time.Second):
		t.Fatal("cancelled gap fetch waited for rpc mutex")
	}
}

func TestFetchRcsGapDropsPageAfterGenerationChanges(t *testing.T) {
	var link *Link
	link = fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		link.mu.Lock()
		link.generation++
		link.mu.Unlock()
		return &gmproto.ListMessagesResponse{Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message}}, nil
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

	if got.Code != "stale_generation" || got.HasMore || got.NextCursor != "" || len(got.Records) != 0 {
		t.Fatalf("result = %#v", got)
	}
}

func TestFetchRcsGapRejectsConversationThatBecomesUnsupportedDuringRPC(t *testing.T) {
	var link *Link
	link = fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		link.mu.Lock()
		link.conversationRecords["conversation-1"].Status = gmproto.ConversationStatus_ARCHIVED
		link.mu.Unlock()
		return &gmproto.ListMessagesResponse{
			Messages: []*gmproto.Message{textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE).Message},
			Cursor:   &gmproto.Cursor{LastItemID: "next", LastItemTimestamp: 1},
		}, nil
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

	if got.Code != "conversation_unsupported" || len(got.Records) != 0 || len(got.Signals) != 0 {
		t.Fatalf("result = %#v", got)
	}
}

func TestFetchRcsGapUsesPerPageDeadline(t *testing.T) {
	previous := rcsGapPageTimeout
	rcsGapPageTimeout = time.Millisecond
	defer func() { rcsGapPageTimeout = previous }()
	link := fetchGapTestLink(func(ctx context.Context, _ string, _ int64, _ *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		<-ctx.Done()
		return nil, ctx.Err()
	})

	got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

	if got.Code != "gap_deadline" || got.HasMore || got.NextCursor != "" || len(got.Records) != 0 {
		t.Fatalf("result = %#v", got)
	}
}

func TestRcsOutputDropsRevokedStreamInsidePlatformDispatch(t *testing.T) {
	host := &ownedRecordHost{}
	link := receiveWiringLink(host)
	output := hostOutput{
		rcsRecord:         `{"code":"ok"}`,
		generation:        7,
		revision:          11,
		streamEpoch:       3,
		requireGeneration: true,
		requireRevision:   true,
		requireStream:     true,
		done:              make(chan struct{}),
	}
	link.outputActive = 1
	link.outputStart = func(hostOutput) {
		link.mu.Lock()
		link.invalidateEvidenceLocked()
		link.ownerVerification = nil
		link.mu.Unlock()
	}

	link.deliverOutput(output)

	if len(host.rcsRecords) != 0 {
		t.Fatalf("records = %v", host.rcsRecords)
	}
}

func TestLinkReceiveRejectsUnsupportedStage2Conversation(t *testing.T) {
	for _, test := range []struct {
		name      string
		configure func(*Link, *gmproto.Conversation)
	}{
		{name: "sms", configure: func(_ *Link, conversation *gmproto.Conversation) { conversation.Type = gmproto.ConversationType_SMS }},
		{name: "inactive", configure: func(_ *Link, conversation *gmproto.Conversation) {
			conversation.Status = gmproto.ConversationStatus_ARCHIVED
		}},
		{name: "group", configure: func(_ *Link, conversation *gmproto.Conversation) {
			conversation.Participants = typingGroup(conversation.ConversationID).Participants
		}},
		{name: "incomplete index", configure: func(link *Link, _ *gmproto.Conversation) { link.conversationIndexComplete = false }},
	} {
		t.Run(test.name, func(t *testing.T) {
			host := &ownedRecordHost{}
			link := receiveWiringLink(host)
			conversation := link.conversationRecords["conversation-1"]
			test.configure(link, conversation)

			link.queueEventFor(7, textMessage(gmproto.MessageStatusType_INCOMING_COMPLETE))
			drainQueuedEvents(link)

			if len(host.rcsRecords) != 1 {
				t.Fatalf("records = %v", host.rcsRecords)
			}
			var projection rcsProjection
			if err := json.Unmarshal([]byte(host.rcsRecords[0]), &projection); err != nil {
				t.Fatal(err)
			}
			if projection.Code != "conversation_unsupported" || projection.Record != nil {
				t.Fatalf("projection = %#v", projection)
			}
		})
	}
}

func TestFetchRcsGapRejectsUnsupportedStage2ConversationBeforeRPC(t *testing.T) {
	for _, test := range []struct {
		name      string
		configure func(*Link, *gmproto.Conversation)
	}{
		{name: "sms", configure: func(_ *Link, conversation *gmproto.Conversation) { conversation.Type = gmproto.ConversationType_SMS }},
		{name: "inactive", configure: func(_ *Link, conversation *gmproto.Conversation) {
			conversation.Status = gmproto.ConversationStatus_ARCHIVED
		}},
		{name: "group", configure: func(_ *Link, conversation *gmproto.Conversation) {
			conversation.Participants = typingGroup(conversation.ConversationID).Participants
		}},
		{name: "incomplete index", configure: func(link *Link, _ *gmproto.Conversation) { link.conversationIndexComplete = false }},
	} {
		t.Run(test.name, func(t *testing.T) {
			calls := 0
			link := fetchGapTestLink(func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
				calls++
				return &gmproto.ListMessagesResponse{}, nil
			})
			conversation := link.conversationRecords["conversation-1"]
			test.configure(link, conversation)

			got := decodeGapResult(t, link.FetchRcsGap("conversation-1", ""))

			if got.Code != "conversation_unsupported" || calls != 0 || len(got.Records) != 0 || len(got.Signals) != 0 {
				t.Fatalf("result = %#v, calls = %d", got, calls)
			}
		})
	}
}

func TestRcsPageProjectionFailuresUseEmptyArrayEnvelope(t *testing.T) {
	conversation := typingConversation("conversation-1", true)
	for _, test := range []struct {
		name string
		page *gmproto.ListMessagesResponse
		code string
	}{
		{name: "missing page", page: nil, code: "page_missing"},
		{name: "missing cursor", page: &gmproto.ListMessagesResponse{Messages: []*gmproto.Message{{ConversationID: "conversation-1"}}}, code: "cursor_missing"},
	} {
		t.Run(test.name, func(t *testing.T) {
			got := projectRcsPage(test.page, conversation)
			if got.Code != test.code || got.Records == nil || got.Signals == nil || len(got.Records) != 0 || len(got.Signals) != 0 {
				t.Fatalf("projection = %#v", got)
			}
			encoded, err := json.Marshal(got)
			if err != nil {
				t.Fatal(err)
			}
			if !strings.Contains(string(encoded), `"records":[]`) || !strings.Contains(string(encoded), `"signals":[]`) {
				t.Fatalf("envelope = %s", encoded)
			}
		})
	}
}
