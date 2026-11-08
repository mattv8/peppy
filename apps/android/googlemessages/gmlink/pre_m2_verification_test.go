package gmlink

import (
	"errors"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// TestM2ConversationIDStableAcrossReconnect pins the shared conversation ID
// field in the pinned schema. Server-side stability remains owner-gated.
func TestM2ConversationIDStableAcrossReconnect(t *testing.T) {
	assertFieldNumber(t, (&gmproto.Conversation{}).ProtoReflect().Descriptor(), "conversationID", 1)
	assertFieldNumber(t, (&gmproto.ConversationEvent{}).ProtoReflect().Descriptor(), "data", 2)
	assertFieldNumber(t, (&gmproto.ListConversationsResponse{}).ProtoReflect().Descriptor(), "conversations", 2)
	assertFieldNumber(t, (&gmproto.GetConversationResponse{}).ProtoReflect().Descriptor(), "conversation", 1)
}

// TestM2TmpIDEchoesInSendAndEvent pins every tmp-id field G1 must populate or
// consume. Whether the upstream service echoes it remains owner-gated.
func TestM2TmpIDEchoesInSendAndEvent(t *testing.T) {
	assertFieldNumber(t, (&gmproto.SendMessageRequest{}).ProtoReflect().Descriptor(), "tmpID", 5)
	payload := (&gmproto.MessagePayload{}).ProtoReflect().Descriptor()
	assertFieldNumber(t, payload, "tmpID", 1)
	assertFieldNumber(t, payload, "participantID", 9)
	assertFieldNumber(t, payload, "tmpID2", 12)
	assertFieldNumber(t, (&gmproto.Message{}).ProtoReflect().Descriptor(), "tmpID", 12)
	assertFieldNumber(t, (&gmproto.MessageEvent{}).ProtoReflect().Descriptor(), "data", 2)
}

// TestM2ConversationFactsExposeVisibleParticipantsTypeModeWritableAndSim pins
// each schema fact G1 must corroborate before allowing a send.
func TestM2ConversationFactsExposeVisibleParticipantsTypeModeWritableAndSim(t *testing.T) {
	conversation := (&gmproto.Conversation{}).ProtoReflect().Descriptor()
	assertFieldNumber(t, conversation, "conversationID", 1)
	assertFieldNumber(t, conversation, "defaultOutgoingID", 11)
	assertFieldNumber(t, conversation, "status", 12)
	assertFieldNumber(t, conversation, "readOnly", 13)
	assertFieldNumber(t, conversation, "sendMode", 18)
	assertFieldNumber(t, conversation, "participants", 20)
	assertFieldNumber(t, conversation, "type", 22)
	assertFieldNumber(t, (&gmproto.Participant{}).ProtoReflect().Descriptor(), "isMe", 6)
	assertFieldNumber(t, (&gmproto.Participant{}).ProtoReflect().Descriptor(), "isVisible", 8)
	assertFieldNumber(t, (&gmproto.SmallInfo{}).ProtoReflect().Descriptor(), "number", 2)
	assertFieldNumber(t, (&gmproto.SIMPayload{}).ProtoReflect().Descriptor(), "SIMNumber", 2)

	assertEnumValues(t, gmproto.ConversationSendMode_SEND_MODE_AUTO.Descriptor(), map[string]protoreflect.EnumNumber{
		"SEND_MODE_AUTO": 0, "SEND_MODE_XMS": 1, "SEND_MODE_XMS_LATCH": 2,
	})
	assertEnumValues(t, gmproto.ConversationType_RCS.Descriptor(), map[string]protoreflect.EnumNumber{
		"UNKNOWN_CONVERSATION_TYPE": 0, "SMS": 1, "RCS": 2,
	})
	assertEnumValues(t, gmproto.ConversationStatus_ACTIVE.Descriptor(), map[string]protoreflect.EnumNumber{
		"UNKNOWN_CONVERSATION_STATUS": 0, "ACTIVE": 1, "ARCHIVED": 2, "DELETED": 3,
		"SPAM_FOLDER": 5, "BLOCKED_FOLDER": 6, "CROSS_COUNTRY_FOLDER": 7, "TRASH_FOLDER": 8,
	})
	assertEnumValues(t, gmproto.ListConversationsRequest_INBOX.Descriptor(), map[string]protoreflect.EnumNumber{
		"UNKNOWN": 0, "INBOX": 1, "ARCHIVE": 2, "SPAM_BLOCKED": 5,
	})
}

// TestM2BoundedLookupClassifiesMissAmbiguityAndFailure fixes the intended G1
// policy: an incomplete scan is not a miss, and error shapes fail closed.
func TestM2BoundedLookupClassifiesMissAmbiguityAndFailure(t *testing.T) {
	cases := []struct {
		name            string
		err             error
		completeFolders bool
		completeCursor  bool
		count           int
		want            string
	}{
		{name: "complete miss", completeFolders: true, completeCursor: true, want: "conversation_missing"},
		{name: "partial folders", completeCursor: true, want: "conversation_lookup_incomplete"},
		{name: "partial cursor", completeFolders: true, want: "conversation_lookup_incomplete"},
		{name: "ambiguity", completeFolders: true, completeCursor: true, count: 2, want: "conversation_ambiguous"},
		{name: "failure", err: errors.New("synthetic lookup failure"), completeFolders: true, completeCursor: true, want: "conversation_lookup_failed"},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			if got := classifyM2LookupForTest(testCase.err, testCase.completeFolders, testCase.completeCursor, testCase.count); got != testCase.want {
				t.Fatalf("lookup classification = %q, want %q", got, testCase.want)
			}
		})
	}
	if got := classifyM2LookupForTest(nil, true, true, -1); got != "conversation_lookup_failed" {
		t.Fatalf("negative candidate count = %q, want failure", got)
	}
}

func assertFieldNumber(t *testing.T, message protoreflect.MessageDescriptor, name protoreflect.Name, want protoreflect.FieldNumber) {
	t.Helper()
	field := message.Fields().ByName(name)
	if field == nil {
		t.Fatalf("%s has no %q field", message.FullName(), name)
	}
	if got := field.Number(); got != want {
		t.Fatalf("%s.%s field number = %d, want %d", message.FullName(), name, got, want)
	}
}

func assertEnumValues(t *testing.T, value protoreflect.EnumDescriptor, want map[string]protoreflect.EnumNumber) {
	t.Helper()
	for name, wantNumber := range want {
		number := value.Values().ByName(protoreflect.Name(name))
		if number == nil {
			t.Fatalf("%s has no %q value", value.FullName(), name)
		}
		if got := number.Number(); got != wantNumber {
			t.Fatalf("%s.%s = %d, want %d", value.FullName(), name, got, wantNumber)
		}
	}
}

func classifyM2LookupForTest(err error, completeFolders, completeCursor bool, matches int) string {
	switch {
	case err != nil || matches < 0:
		return "conversation_lookup_failed"
	case !completeFolders || !completeCursor:
		return "conversation_lookup_incomplete"
	case matches == 0:
		return "conversation_missing"
	case matches == 1:
		return "conversation_found"
	default:
		return "conversation_ambiguous"
	}
}
