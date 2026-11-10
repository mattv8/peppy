package gmlink

import (
	"encoding/json"
	"strconv"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestConversationFacadeRequiresVerification(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	if result := link.RefreshConversationIndex(); result != `{"code":"unavailable"}` {
		t.Fatalf("RefreshConversationIndex result = %s", result)
	}
	for _, result := range []string{link.GetConversation("synthetic-conversation"), link.ResolveExistingConversation("synthetic-target")} {
		if !strings.Contains(result, `"code":"m2_verification_required"`) {
			t.Fatalf("result = %s", result)
		}
	}
}

func TestConversationCandidateSnapshotIncludesGroupRosterFacts(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	setTestOwnerVerification(link)
	stage2SeedCompleteIndex(t, link, &gmproto.Conversation{
		ConversationID: "group", Name: "Weekend", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		Participants: []*gmproto.Participant{
			{IsVisible: true, IsMe: true, ID: &gmproto.SmallInfo{Number: "+15551230000"}},
			{IsVisible: true, FullName: "Alex", ID: &gmproto.SmallInfo{Number: "+15551230001"}},
			{IsVisible: false, ID: &gmproto.SmallInfo{Number: "+15551230002"}},
		},
	})
	var snapshot struct {
		Conversations []conversationCandidateRecord `json:"conversations"`
	}
	if err := json.Unmarshal([]byte(link.ConversationCandidateSnapshot()), &snapshot); err != nil {
		t.Fatal(err)
	}
	if len(snapshot.Conversations) != 1 {
		t.Fatalf("conversations = %d", len(snapshot.Conversations))
	}
	record := snapshot.Conversations[0]
	if !record.IsGroupChat || record.Name != "Weekend" || record.InvisibleParticipantCount != 1 || record.MemberDigest == "" || record.Participants[1].CanonicalAddress != "+15551230001" || !record.Participants[1].CanonicalAddressPresent || record.Participants[1].DisplayName != "Alex" || record.Participants[2].CanonicalAddressPresent {
		t.Fatalf("record = %#v", record)
	}
}

func TestRefreshConversationIndexUsesClosedCodes(t *testing.T) {
	for refusal, want := range map[string]string{
		"":                            "ok",
		"pending_conversation_events": "pending",
		"conversation_lookup_stale":   "stale",
		"m2_verification_required":    "unavailable",
		"cancelled":                   "cancelled",
	} {
		if got := refreshConversationIndexJSON(refusal); got != `{"code":"`+want+`"}` {
			t.Fatalf("refusal %q = %s, want %q", refusal, got, want)
		}
	}
}

func TestDrainConversationEventsExcludesParticipantIdentity(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	setTestOwnerVerification(link)
	link.queueEvent(&gmproto.Conversation{
		ConversationID: "synthetic-conversation",
		Participants:   []*gmproto.Participant{{ID: &gmproto.SmallInfo{Number: "synthetic-target"}}},
	})
	awaitQueued(t, "conversation ingress", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return len(link.conversationEvents) == 1
	})
	result := link.DrainConversationEvents()
	if strings.Contains(result, "synthetic-target") || !strings.Contains(result, "synthetic-conversation") {
		t.Fatalf("event result = %s", result)
	}
}

func TestResolveExistingConversationNeverReturnsParticipantIdentity(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	result := link.ResolveExistingConversation("synthetic-target")
	var decoded map[string]any
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	if _, exists := decoded["participant"]; exists {
		t.Fatalf("participant identity leaked in %s", result)
	}
}

func TestVisibleOtherParticipantsCountsNumberlessParticipant(t *testing.T) {
	conversation := &gmproto.Conversation{Participants: []*gmproto.Participant{
		{IsMe: true, IsVisible: true},
		{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}},
		{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-other"}},
		{IsVisible: false, ID: &gmproto.SmallInfo{Number: "ignored-target"}},
	}}
	if got := len(visibleOtherParticipants(conversation)); got != 2 {
		t.Fatalf("visible other participants = %d, want 2", got)
	}
}

func TestConversationEventsCoalesceAndRequestResyncAtCapacity(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.recordConversationEventLocked("synthetic-conversation")
	link.recordConversationEventLocked("synthetic-conversation")
	if got := len(link.conversationEvents); got != 1 {
		link.mu.Unlock()
		t.Fatalf("coalesced events = %d, want 1", got)
	}
	for index := 1; index <= conversationEventLimit; index++ {
		link.recordConversationEventLocked("synthetic-conversation-" + strconv.Itoa(index))
	}
	events := append([]conversationEvent(nil), link.conversationEvents...)
	link.mu.Unlock()
	if len(events) != 1 || events[0].Code != "conversation_resync" || events[0].GoogleConversationID != "" {
		t.Fatalf("overflow events = %+v", events)
	}
}
