package gmlink

import (
	"context"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
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

func typingLink(host Host, conversation *gmproto.Conversation) *Link {
	return &Link{
		host:              host,
		client:            &libgm.Client{},
		state:             stateLinked,
		running:           true,
		runCtx:            context.Background(),
		generation:        7,
		runGeneration:     7,
		streamEpoch:       3,
		stateRevision:     11,
		ownerVerification: &ownerVerification{generation: 7},
		changes:           make(chan any, 8),
		conversationRecords: map[string]*gmproto.Conversation{
			conversation.GetConversationID(): conversation,
		},
	}
}

func typingData() *gmproto.TypingData {
	return &gmproto.TypingData{
		ConversationID: "synthetic-conversation",
		User:           &gmproto.User{Number: "synthetic-participant"},
		Type:           gmproto.TypingTypes_STARTED_TYPING,
	}
}

func TestLinkProjectsIncomingTypingForCurrentOwner(t *testing.T) {
	host := &ownedRecordHost{}
	link := typingLink(host, typingConversation("synthetic-conversation", true))

	link.queueEventFor(7, typingData())
	drainQueuedEvents(link)

	if len(host.typingRecords) != 1 || host.typingRecords[0] != `{"google_conversation_id":"synthetic-conversation","state":"started"}` {
		t.Fatalf("records = %v", host.typingRecords)
	}
}

func TestLinkIgnoresInvisibleGroupAndOldGenerationTyping(t *testing.T) {
	for _, test := range []struct {
		name         string
		conversation *gmproto.Conversation
		generation   uint64
	}{
		{name: "invisible", conversation: typingConversation("synthetic-conversation", false), generation: 7},
		{name: "group", conversation: typingGroup("synthetic-conversation"), generation: 7},
		{name: "old generation", conversation: typingConversation("synthetic-conversation", true), generation: 6},
	} {
		t.Run(test.name, func(t *testing.T) {
			host := &ownedRecordHost{}
			link := typingLink(host, test.conversation)
			link.queueEventFor(test.generation, typingData())
			drainQueuedEvents(link)
			if len(host.typingRecords) != 0 {
				t.Fatalf("records = %v", host.typingRecords)
			}
		})
	}
}

func TestSetTypingRefusesExpiredAndOldGeneration(t *testing.T) {
	conversation := typingConversation("synthetic-conversation", true)
	conversation.DefaultOutgoingID = "self-sim"
	link := typingLink(&ownedRecordHost{}, conversation)
	link.settings = typingSettings()
	link.typingNowMono = func() uint64 { return 100 }
	calls := 0
	link.setTyping = func(context.Context, string, *gmproto.SIMPayload) error { calls++; return nil }

	if got := link.SetTyping(`{"conversation_id":"synthetic-conversation","state":"started","deadline_mono":100}`); got != "typing_expired" {
		t.Fatalf("expired = %q", got)
	}
	link.runGeneration = 6
	if got := link.SetTyping(`{"conversation_id":"synthetic-conversation","state":"started","deadline_mono":101}`); got != "typing_stale" {
		t.Fatalf("stale = %q", got)
	}
	if calls != 0 {
		t.Fatalf("calls = %d", calls)
	}
}

func TestSetTypingStartsWithBoundedContext(t *testing.T) {
	conversation := typingConversation("synthetic-conversation", true)
	conversation.DefaultOutgoingID = "self-sim"
	link := typingLink(&ownedRecordHost{}, conversation)
	link.settings = typingSettings()
	link.typingNowMono = func() uint64 { return 100 }
	called := false
	link.setTyping = func(ctx context.Context, conversationID string, payload *gmproto.SIMPayload) error {
		called = true
		deadline, ok := ctx.Deadline()
		if !ok || time.Until(deadline) <= 0 || time.Until(deadline) > typingRequestTimeout {
			t.Fatalf("deadline = %v, bounded = %t", deadline, ok)
		}
		if conversationID != "synthetic-conversation" || payload.GetSIMNumber() != 1 {
			t.Fatalf("arguments = %q, %#v", conversationID, payload)
		}
		return nil
	}

	if got := link.SetTyping(`{"conversation_id":"synthetic-conversation","state":"started","deadline_mono":101}`); got != "ok" || !called {
		t.Fatalf("result = %q, called = %t", got, called)
	}
}

func typingSettings() *gmproto.Settings {
	return &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		SIMParticipant: &gmproto.SIMParticipant{ID: "self-sim"},
		RCSChats:       &gmproto.RCSChats{Enabled: true},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}},
	}}}
}
