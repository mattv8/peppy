package gmlink

import (
	"strings"
	"testing"
	"time"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func TestConversationIndexRefusesAmbiguousOrIneligibleCandidates(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		&gmproto.Conversation{ConversationID: "synthetic-first", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}}},
		&gmproto.Conversation{ConversationID: "synthetic-second", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-target"}}}},
	)
	if result := link.ResolveExistingConversation("synthetic-target"); !strings.Contains(result, `"code":"conversation_ambiguous"`) {
		t.Fatalf("ambiguous candidates resolved: %s", result)
	}
	for name, conversation := range map[string]*gmproto.Conversation{
		"sms":        {ConversationID: "synthetic-sms", Type: gmproto.ConversationType_SMS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-sms-target"}}}},
		"archived":   {ConversationID: "synthetic-archived", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ARCHIVED, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-archived-target"}}}},
		"group":      {ConversationID: "synthetic-group", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-group-target"}}, {IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-group-other"}}}},
		"numberless": {ConversationID: "synthetic-numberless", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-numberless-target"}}, {IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "synthetic-numberless"}}}},
	} {
		t.Run(name, func(t *testing.T) {
			stage2SeedCompleteIndex(t, link, conversation)
			target := conversation.GetParticipants()[0].GetID().GetNumber()
			want := `"code":"conversation_not_found_complete"`
			if name == "group" || name == "numberless" {
				want = `"code":"conversation_lookup_unknown"`
			}
			if result := link.ResolveExistingConversation(target); !strings.Contains(result, want) {
				t.Fatalf("ineligible conversation result: %s", result)
			}
		})
	}
	stage2SeedCompleteIndex(t, link, &gmproto.Conversation{ConversationID: "synthetic-replaced", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-old"}}}})
	link.indexConversations([]*gmproto.Conversation{{ConversationID: "synthetic-replaced", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-new"}}}}})
	if result := link.ResolveExistingConversation("synthetic-old"); !strings.Contains(result, `"code":"conversation_lookup_unknown"`) {
		t.Fatalf("replaced association survived: %s", result)
	}
	stage2SeedCompleteIndex(t, link, &gmproto.Conversation{ConversationID: "synthetic-hidden-extra", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-visible"}}, {IsVisible: false, ID: &gmproto.SmallInfo{Number: "synthetic-hidden"}}}})
	if result := link.ResolveExistingConversation("synthetic-visible"); !strings.Contains(result, `"code":"conversation_found"`) {
		t.Fatalf("hidden participant made candidate ineligible: %s", result)
	}
}

func TestRefreshConversationIndexRescansAfterIncrementalUpdate(t *testing.T) {
	auth := linkedSafetyAuth()
	link, _ := openSafetyLink(t, nil)
	setSafetyState(link, stateLinked, auth)
	fixture := newPhoneFixture(t, auth)
	fixture.configure(func(knobs *fixtureKnobs) {
		knobs.configEmail = fixtureSourceID
		knobs.gateList = true
	})
	run := make(chan error, 1)
	go func() { run <- link.Run() }()
	fixture.awaitStreamReady(t)
	awaitStreamOpen(t, link)
	setTestOwnerVerification(link)
	result := make(chan string, 1)
	go func() { result <- link.RefreshConversationIndex() }()
	select {
	case folder := <-fixture.firstListFolder:
		if folder != gmproto.ListConversationsRequest_INBOX {
			t.Fatalf("first refresh folder = %v, want INBOX", folder)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("refresh did not reach first folder")
	}
	link.mu.Lock()
	generation, source := link.generation, link.streamSource
	link.mu.Unlock()
	link.queueEventFor(generation, &libgm.StreamEvent{Source: source, Event: &gmproto.Conversation{ConversationID: "synthetic-incremental", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: []*gmproto.Participant{{IsVisible: true, ID: &gmproto.SmallInfo{Number: "synthetic-incremental-target"}}}}})
	awaitSafety(t, "stream conversation event projection", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return len(link.conversationCandidates["synthetic-incremental-target"]) == 1
	})
	close(fixture.listRelease)
	select {
	case refreshResult := <-result:
		if refreshResult != `{"code":"ok"}` {
			t.Fatalf("RefreshConversationIndex result = %s", refreshResult)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("RefreshConversationIndex did not return")
	}
	// The superseded scan is discarded: the published index is the rescan, which the phone
	// answered after the update, so the update's own record is gone with the stale scan.
	if requests := len(fixture.listRequests()); requests != 4 {
		t.Fatalf("list requests = %d, want the abandoned first page and one three-folder rescan", requests)
	}
	link.mu.Lock()
	complete, stalePublished := link.conversationIndexCurrentLocked(), len(link.conversationCandidates["synthetic-incremental-target"]) != 0
	link.mu.Unlock()
	if !complete || stalePublished {
		t.Fatalf("complete=%t stalePublished=%t", complete, stalePublished)
	}
	link.Stop()
	select {
	case err := <-run:
		if err != nil {
			t.Fatalf("Run error = %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not return")
	}
}
