package gmlink

import (
	"context"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestTypingProjectionMapsPinnedStatesForVisibleParticipant(t *testing.T) {
	conversation := typingConversation("synthetic-conversation", true)
	for _, test := range []struct {
		name string
		kind gmproto.TypingTypes
		want string
	}{
		{name: "started", kind: gmproto.TypingTypes_STARTED_TYPING, want: "started"},
		{name: "stopped", kind: gmproto.TypingTypes_STOPPED_TYPING, want: "stopped"},
	} {
		t.Run(test.name, func(t *testing.T) {
			result := projectTypingData(&gmproto.TypingData{
				ConversationID: "synthetic-conversation",
				User:           &gmproto.User{Number: "synthetic-participant"},
				Type:           test.kind,
			}, false, []*gmproto.Conversation{conversation})
			if result.Code != "ok" || result.Record == nil {
				t.Fatalf("projection = %+v", result)
			}
			if result.Record.GoogleConversationID != "synthetic-conversation" || result.Record.State != test.want {
				t.Fatalf("record = %+v", result.Record)
			}
			assertTypingRecordShape(t, result.Record)
		})
	}
}

func TestTypingProjectionRefusesOldUnknownAndInvisibleParticipants(t *testing.T) {
	data := &gmproto.TypingData{ConversationID: "synthetic-conversation", User: &gmproto.User{Number: "synthetic-participant"}, Type: gmproto.TypingTypes_STARTED_TYPING}
	for _, test := range []struct {
		name          string
		old           bool
		conversations []*gmproto.Conversation
		want          string
	}{
		{name: "old", old: true, conversations: []*gmproto.Conversation{typingConversation("synthetic-conversation", true)}, want: "typing_ignored"},
		{name: "unknown conversation", conversations: []*gmproto.Conversation{typingConversation("other-conversation", true)}, want: "typing_ignored"},
		{name: "invisible participant", conversations: []*gmproto.Conversation{typingConversation("synthetic-conversation", false)}, want: "typing_ignored"},
	} {
		t.Run(test.name, func(t *testing.T) {
			result := projectTypingData(data, test.old, test.conversations)
			if result.Code != test.want || result.Record != nil {
				t.Fatalf("projection = %+v", result)
			}
		})
	}
}

func TestTypingArgumentsRefuseExpiredUnknownAndGroup(t *testing.T) {
	intent := gatewayTypingIntent{ConversationID: "synthetic-conversation", State: typingStateStarted, DeadlineMono: 101}
	for _, test := range []struct {
		name          string
		intent        gatewayTypingIntent
		conversations []*gmproto.Conversation
		now           uint64
		want          string
	}{
		{name: "expired", intent: intent, conversations: []*gmproto.Conversation{typingConversation("synthetic-conversation", true)}, now: 101, want: "typing_expired"},
		{name: "unknown", intent: intent, conversations: []*gmproto.Conversation{typingConversation("other-conversation", true)}, now: 100, want: "typing_unknown_conversation"},
		{name: "group", intent: intent, conversations: []*gmproto.Conversation{typingGroup("synthetic-conversation")}, now: 100, want: "typing_not_one_to_one"},
	} {
		t.Run(test.name, func(t *testing.T) {
			arguments, code := typingSetArguments(test.intent, test.now, test.conversations, &gmproto.SIMPayload{SIMNumber: 1})
			if arguments != nil || code != test.want {
				t.Fatalf("arguments = %+v code = %q", arguments, code)
			}
		})
	}
}

func TestTypingArgumentsOnlyStartsAndContextIsBounded(t *testing.T) {
	conversation := typingConversation("synthetic-conversation", true)
	arguments, code := typingSetArguments(gatewayTypingIntent{ConversationID: "synthetic-conversation", State: typingStateStarted, DeadlineMono: 101}, 100, []*gmproto.Conversation{conversation}, &gmproto.SIMPayload{SIMNumber: 1})
	if code != "ok" || arguments == nil || arguments.ConversationID != "synthetic-conversation" || arguments.SIMPayload == nil {
		t.Fatalf("arguments = %+v code = %q", arguments, code)
	}
	arguments, code = typingSetArguments(gatewayTypingIntent{ConversationID: "synthetic-conversation", State: typingStateStopped, DeadlineMono: 101}, 100, []*gmproto.Conversation{conversation}, &gmproto.SIMPayload{SIMNumber: 1})
	if arguments != nil || code != "typing_stopped" {
		t.Fatalf("arguments = %+v code = %q", arguments, code)
	}

	ctx, cancel := typingContext(context.Background())
	defer cancel()
	deadline, ok := ctx.Deadline()
	if !ok || time.Until(deadline) > typingRequestTimeout || time.Until(deadline) <= 0 {
		t.Fatalf("deadline = %v bounded = %t", deadline, ok)
	}
}

func typingConversation(id string, visible bool) *gmproto.Conversation {
	return &gmproto.Conversation{ConversationID: id, Participants: []*gmproto.Participant{
		{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-self"}},
		{IsVisible: visible, ID: &gmproto.SmallInfo{Number: "synthetic-participant"}},
	}}
}

func typingGroup(id string) *gmproto.Conversation {
	conversation := typingConversation(id, true)
	conversation.Participants = append(conversation.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-other"}})
	return conversation
}

func assertTypingRecordShape(t *testing.T, record *typingRecord) {
	t.Helper()
	if record == nil || record.GoogleConversationID == "" || (record.State != "started" && record.State != "stopped") {
		t.Fatalf("invalid record = %+v", record)
	}
}
