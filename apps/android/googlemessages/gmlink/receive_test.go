package gmlink

import (
	"encoding/json"
	"fmt"
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
		Timestamp:      1735689600123456,
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

func TestRcsProjectionRejectsUnsupportedAndUnknownMessages(t *testing.T) {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS}
	media := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234,
		MessageStatus: &gmproto.MessageStatus{Status: gmproto.MessageStatusType_INCOMING_COMPLETE},
		MessageInfo:   []*gmproto.MessageInfo{{Data: &gmproto.MessageInfo_MediaContent{MediaContent: &gmproto.MediaContent{}}}},
	}}
	if got := projectRcsMessage(media, conversation); got.Code != "unsupported_message" || got.Record != nil {
		t.Fatalf("media projection = %#v", got)
	} else {
		assertRejectedProjectionHasNoBody(t, got)
	}

	unknown := &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1234,
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
	if withoutResponseCursor.Records[0].Cursor != withoutResponseCursor.NextCursor {
		t.Fatalf("record cursor = %q, next cursor = %q", withoutResponseCursor.Records[0].Cursor, withoutResponseCursor.NextCursor)
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
		{"encrypted text", gmproto.MessageStatusType_MESSAGE_STATUS_TOMBSTONE_PROTOCOL_SWITCH_E2EE_TO_TEXT},
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

func textMessage(status gmproto.MessageStatusType) *libgm.WrappedMessage {
	return &libgm.WrappedMessage{Message: &gmproto.Message{
		MessageID: "message-1", ConversationID: "conversation-1", Timestamp: 1735689600123456,
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
	rcsRecords    []string
	typingRecords []string
}

func (*ownedRecordHost) PersistState([]byte) error                 { return nil }
func (*ownedRecordHost) Event(string, string, int64, int64, int64) {}
func (*ownedRecordHost) ShowEmoji(string, int64, int64)            {}
func (host *ownedRecordHost) RcsEvent(record string, _, _ int64) {
	host.rcsRecords = append(host.rcsRecords, record)
}
func (host *ownedRecordHost) TypingEvent(record string, _, _ int64) {
	host.typingRecords = append(host.typingRecords, record)
}

func receiveWiringLink(host Host) *Link {
	conversation := &gmproto.Conversation{ConversationID: "conversation-1", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE}
	return &Link{
		host:                host,
		generation:          7,
		runGeneration:       7,
		streamEpoch:         3,
		stateRevision:       11,
		ownerVerification:   &ownerVerification{generation: 7},
		changes:             make(chan any, 8),
		conversationRecords: map[string]*gmproto.Conversation{"conversation-1": conversation},
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
		MessageID: "media", ConversationID: "conversation-1", Timestamp: 1,
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
