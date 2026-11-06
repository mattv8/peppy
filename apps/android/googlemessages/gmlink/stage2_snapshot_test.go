package gmlink

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func stage2Participant(number string, visible, me bool) *gmproto.Participant {
	return &gmproto.Participant{IsVisible: visible, IsMe: me, ID: &gmproto.SmallInfo{Number: number}}
}

func stage2Conversation(id string, participants ...*gmproto.Participant) *gmproto.Conversation {
	return &gmproto.Conversation{ConversationID: id, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, Participants: participants}
}

func stage2Code(t *testing.T, result string) string {
	t.Helper()
	var decoded conversationResult
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	return decoded.Code
}

func stage2SeedCompleteIndex(t *testing.T, link *Link, conversations ...*gmproto.Conversation) (uint64, uint64) {
	t.Helper()
	link.mu.Lock()
	link.generation, link.runGeneration, link.streamEpoch = 7, 7, 3
	link.ownerVerification = &ownerVerification{generation: 7}
	link.mu.Unlock()
	revision, ok := link.beginConversationIndexRefreshFor(7, 3)
	if !ok || !link.replaceConversationIndexFor(7, 3, revision, conversations) {
		t.Fatal("complete conversation index was not published")
	}
	return 7, 3
}

func TestConversationLookupRequiresCompleteSnapshot(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	setTestOwnerVerification(link)
	if code := stage2Code(t, link.ResolveExistingConversation("missing-target")); code != "conversation_lookup_unknown" {
		t.Fatalf("lookup code = %s", code)
	}
	stage2SeedCompleteIndex(t, link)
	if code := stage2Code(t, link.ResolveExistingConversation("missing-target")); code != "conversation_not_found_complete" {
		t.Fatalf("complete miss code = %s", code)
	}
}

func TestPartialOrCancelledScanNeverProvesCompleteAbsence(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	if _, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch); !ok {
		t.Fatal("refresh did not start")
	}
	calls := 0
	_, code := scanConversationPages(context.Background(), func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		if calls == 1 {
			return &gmproto.ListConversationsResponse{Cursor: &gmproto.Cursor{LastItemID: "next"}}, nil
		}
		return nil, context.Canceled
	})
	if code != "conversation_lookup_failed" {
		t.Fatalf("scan code = %s", code)
	}
	if code := stage2Code(t, link.ResolveExistingConversation("missing-target")); code != "conversation_lookup_unknown" {
		t.Fatalf("partial scan lookup code = %s", code)
	}
}

func TestRepeatedCursorIsBoundedIncomplete(t *testing.T) {
	cursor := &gmproto.Cursor{LastItemID: "same", LastItemTimestamp: 42}
	_, code := scanConversationPages(context.Background(), func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		return &gmproto.ListConversationsResponse{Cursor: cursor}, nil
	})
	if code != "conversation_lookup_incomplete" {
		t.Fatalf("scan code = %s", code)
	}
}

func TestMultiPageScanPreservesDistinctCandidates(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	link.mu.Lock()
	link.generation, link.runGeneration, link.streamEpoch = 7, 7, 3
	link.ownerVerification = &ownerVerification{generation: 7}
	link.mu.Unlock()
	revision, ok := link.beginConversationIndexRefreshFor(7, 3)
	if !ok {
		t.Fatal("refresh did not start")
	}
	calls := make(map[gmproto.ListConversationsRequest_Folder]int)
	conversations, code := scanConversationPages(context.Background(), func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		folder := request.GetFolder()
		calls[folder]++
		if folder != gmproto.ListConversationsRequest_INBOX {
			return &gmproto.ListConversationsResponse{}, nil
		}
		if calls[folder] == 1 {
			return &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{stage2Conversation("first", stage2Participant("first-target", true, false))}, Cursor: &gmproto.Cursor{LastItemID: "next"}}, nil
		}
		return &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{
			stage2Conversation("second", stage2Participant("second-target", true, false)),
			stage2Conversation("first", stage2Participant("updated-target", true, false)),
		}}, nil
	})
	if code != "" || !link.replaceConversationIndexFor(7, 3, revision, conversations) {
		t.Fatalf("scan code = %s", code)
	}
	link.mu.Lock()
	updated := append([]conversationSummary(nil), link.conversationCandidates["updated-target"]...)
	second := append([]conversationSummary(nil), link.conversationCandidates["second-target"]...)
	_, stale := link.conversationCandidates["first-target"]
	link.mu.Unlock()
	if len(updated) != 1 || updated[0].id != "first" || len(second) != 1 || second[0].id != "second" || stale {
		t.Fatalf("multi-page candidates not preserved: updated=%+v second=%+v stale=%t", updated, second, stale)
	}
}

func TestOldGenerationScanCannotPublishCompleteSnapshot(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	revision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok {
		t.Fatal("refresh did not start")
	}
	link.mu.Lock()
	link.revokeLocked()
	link.ownerVerification = &ownerVerification{generation: link.generation}
	link.mu.Unlock()
	if link.replaceConversationIndexFor(generation, streamEpoch, revision, []*gmproto.Conversation{stage2Conversation("stale", stage2Participant("stale-target", true, false))}) {
		t.Fatal("old generation published a complete snapshot")
	}
	if code := stage2Code(t, link.ResolveExistingConversation("stale-target")); code != "conversation_lookup_unknown" {
		t.Fatalf("lookup code = %s", code)
	}
}

func TestStreamGapInvalidatesCompleteSnapshot(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, _ := stage2SeedCompleteIndex(t, link)
	link.queueEventFor(generation, &events.StreamGap{})
	link.mu.Lock()
	complete := link.conversationIndexComplete
	link.mu.Unlock()
	if complete {
		t.Fatal("stream gap retained complete snapshot")
	}
}

func TestCorrelationOverflowIsStickyAndInvalidatesSnapshot(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link)
	link.mu.Lock()
	for index := 0; index <= conversationEventLimit; index++ {
		link.recordConversationEventLocked(fmt.Sprintf("conversation-%d", index))
	}
	link.recordConversationEventLocked("after-overflow")
	complete := link.conversationIndexComplete
	events := append([]conversationEvent(nil), link.conversationEvents...)
	link.mu.Unlock()
	if complete {
		t.Fatal("correlation overflow retained complete snapshot")
	}
	if len(events) != 1 || events[0].Code != "conversation_resync" || events[0].GoogleConversationID != "" {
		t.Fatalf("overflow events = %+v", events)
	}
}

func TestExactTargetIgnoresGroupAndNonVisibleParticipants(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		stage2Conversation("group", stage2Participant("target", true, false), stage2Participant("other", true, false)),
		stage2Conversation("hidden", stage2Participant("target", false, false), stage2Participant("other", true, false)),
	)
	if code := stage2Code(t, link.ResolveExistingConversation("target")); code != "conversation_lookup_unknown" {
		t.Fatalf("ineligible target code = %s", code)
	}
	revision, ok := link.beginConversationIndexRefreshFor(7, 3)
	if !ok || !link.replaceConversationIndexFor(7, 3, revision, []*gmproto.Conversation{
		stage2Conversation("exact", stage2Participant("", true, true), stage2Participant("target", true, false), stage2Participant("ignored", false, false)),
	}) {
		t.Fatal("exact snapshot was not published")
	}
	result := link.ResolveExistingConversation("target")
	var decoded conversationResult
	if err := json.Unmarshal([]byte(result), &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Code != "conversation_found" || decoded.GoogleConversationID != "exact" {
		t.Fatalf("lookup result = %s", result)
	}
}

func TestPublishedCandidatesOwnClonedRawFacts(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	conversation := stage2Conversation("exact", stage2Participant("original-target", true, false))
	stage2SeedCompleteIndex(t, link, conversation)
	conversation.Participants[0].ID.Number = "mutated-target"
	link.mu.Lock()
	summary := link.conversationCandidates["original-target"][0]
	raw := summary.conversation.GetParticipants()[0].GetID().GetNumber()
	link.mu.Unlock()
	if raw != "original-target" {
		t.Fatalf("stored raw participant = %q", raw)
	}
}

func TestSnapshotRemainsConsistentUnderConcurrentEvents(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link,
		stage2Conversation("stable", stage2Participant("stable-target", true, false)),
		stage2Conversation("changing", stage2Participant("target-a", true, false)),
	)
	var wait sync.WaitGroup
	failures := make(chan string, 1)
	wait.Add(2)
	go func() {
		defer wait.Done()
		for index := 0; index < conversationEventLimit/2; index++ {
			target := "target-a"
			if index%2 == 0 {
				target = "target-b"
			}
			link.recordConversationEventFor(generation, libgm.StreamSource{}, streamEpoch, stage2Conversation("changing", stage2Participant(target, true, false)))
		}
	}()
	go func() {
		defer wait.Done()
		for index := 0; index < 500; index++ {
			var result conversationResult
			err := json.Unmarshal([]byte(link.ResolveExistingConversation("stable-target")), &result)
			if err != nil || (result.Code != "conversation_found" && result.Code != "conversation_lookup_unknown") {
				select {
				case failures <- fmt.Sprintf("stable lookup = %+v error=%v", result, err):
				default:
				}
				return
			}
		}
	}()
	wait.Wait()
	select {
	case failure := <-failures:
		t.Fatal(failure)
	default:
	}
}

func TestConversationItemOverflowIsBoundedIncomplete(t *testing.T) {
	conversations := make([]*gmproto.Conversation, 0, conversationItemLimit+1)
	for index := 0; index <= conversationItemLimit; index++ {
		conversations = append(conversations, stage2Conversation(fmt.Sprintf("conversation-%d", index), stage2Participant(fmt.Sprintf("target-%d", index), true, false)))
	}
	calls := 0
	_, code := scanConversationPages(context.Background(), func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		if calls == 1 {
			return &gmproto.ListConversationsResponse{Conversations: conversations}, nil
		}
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "conversation_lookup_incomplete" {
		t.Fatalf("scan code = %s", code)
	}
}

func TestDirectGapSignalsInvalidateCompleteSnapshot(t *testing.T) {
	for name, event := range map[string]any{
		"stream gap":        &events.StreamGap{},
		"temporary error":   &events.ListenTemporaryError{},
		"no data":           &events.NoDataReceived{},
		"session reset":     &events.ActiveSessionSet{},
		"phone unavailable": &events.PhoneNotResponding{},
		"bare settings":     &gmproto.Settings{},
	} {
		t.Run(name, func(t *testing.T) {
			link, _ := openSafetyLink(t, nil)
			stage2SeedCompleteIndex(t, link)
			link.projectEvent(event)
			link.mu.Lock()
			complete := link.conversationIndexComplete
			link.mu.Unlock()
			if complete {
				t.Fatal("gap signal retained complete snapshot")
			}
		})
	}
}

func stage2LinkWithoutEventConsumer() *Link {
	return &Link{
		changes:                make(chan any, 64),
		conversationCandidates: make(map[string][]conversationSummary),
		conversationTargets:    make(map[string][]string),
	}
}

func TestAcceptedConversationEventFencesScanBeforeConsumerDrain(t *testing.T) {
	link := stage2LinkWithoutEventConsumer()
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	revision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok {
		t.Fatal("refresh did not start")
	}
	link.queueEventFor(generation, stage2Conversation("contradiction", stage2Participant("target", true, false)))
	if link.replaceConversationIndexFor(generation, streamEpoch, revision, nil) {
		t.Fatal("scan published while an accepted conversation event remained unapplied")
	}
	if code := stage2Code(t, link.ResolveExistingConversation("target")); code != "conversation_lookup_unknown" {
		t.Fatalf("lookup code = %s", code)
	}
}

func TestQueuedPreScanConversationCannotOverwriteNewerSnapshot(t *testing.T) {
	link := stage2LinkWithoutEventConsumer()
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	link.queueEventFor(generation, stage2Conversation("same-id", stage2Participant("old-target", true, false)))
	revision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok {
		t.Fatal("refresh did not start")
	}
	if link.replaceConversationIndexFor(generation, streamEpoch, revision, []*gmproto.Conversation{
		stage2Conversation("same-id", stage2Participant("new-target", true, false)),
	}) {
		t.Fatal("scan published over an unapplied pre-scan event")
	}
	link.consumeQueuedEvent(<-link.changes)
	if link.replaceConversationIndexFor(generation, streamEpoch, revision, nil) {
		t.Fatal("scan published after its revision was superseded by the queued event")
	}
	freshRevision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok || !link.replaceConversationIndexFor(generation, streamEpoch, freshRevision, []*gmproto.Conversation{
		stage2Conversation("same-id", stage2Participant("new-target", true, false)),
	}) {
		t.Fatal("newer snapshot did not publish after the queue drained")
	}
	if code := stage2Code(t, link.ResolveExistingConversation("old-target")); code != "conversation_lookup_unknown" {
		t.Fatalf("old target lookup code = %s", code)
	}
	if code := stage2Code(t, link.ResolveExistingConversation("new-target")); code != "conversation_found" {
		t.Fatalf("new target lookup code = %s", code)
	}
}

func TestEquivalentNumberFormatsNeverProveCompleteAbsenceOrUniqueness(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		stage2Conversation("formatted", stage2Participant("(202) 555-0123", true, false)),
	)
	if code := stage2Code(t, link.ResolveExistingConversation("+12025550123")); code != "conversation_lookup_unknown" {
		t.Fatalf("equivalent-format miss code = %s", code)
	}
	stage2SeedCompleteIndex(t, link,
		stage2Conversation("exact", stage2Participant("+12025550123", true, false)),
		stage2Conversation("formatted", stage2Participant("(202) 555-0123", true, false)),
	)
	if code := stage2Code(t, link.ResolveExistingConversation("+12025550123")); code != "conversation_lookup_unknown" {
		t.Fatalf("potential false uniqueness code = %s", code)
	}
}

func TestFinalPageSuccessAfterCancellationIsRejected(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	calls := 0
	_, code := scanConversationPages(ctx, func(_ context.Context, _ *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		calls++
		if calls == 3 {
			cancel()
		}
		return &gmproto.ListConversationsResponse{}, nil
	})
	if code != "conversation_lookup_failed" {
		t.Fatalf("scan code = %s", code)
	}
}

func TestCancellationAtPublicationBoundaryRejectsSnapshot(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	revision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok {
		t.Fatal("refresh did not start")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if link.replaceConversationIndexForContext(ctx, generation, streamEpoch, revision, nil) {
		t.Fatal("cancelled operation published a complete snapshot")
	}
	if code := stage2Code(t, link.ResolveExistingConversation("target")); code != "conversation_lookup_unknown" {
		t.Fatalf("lookup code = %s", code)
	}
}

func TestSelfChatIsUnsupportedWithoutCarrierProof(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	stage2SeedCompleteIndex(t, link,
		stage2Conversation("self-chat", stage2Participant("own-target", true, true)),
	)
	if code := stage2Code(t, link.ResolveExistingConversation("own-target")); code != "conversation_unsupported" {
		t.Fatalf("self-chat lookup code = %s", code)
	}
	link.mu.Lock()
	summaries := append([]conversationSummary(nil), link.conversationCandidates["own-target"]...)
	link.mu.Unlock()
	if len(summaries) != 1 || !summaries[0].self || summaries[0].conversation == nil {
		t.Fatalf("self-chat facts were not preserved: %+v", summaries)
	}
}

func TestConversationResyncRemainsStickyAcrossDrainsUntilReplacement(t *testing.T) {
	link, _ := openSafetyLink(t, nil)
	generation, streamEpoch := stage2SeedCompleteIndex(t, link)
	link.mu.Lock()
	for index := 0; index <= conversationEventLimit; index++ {
		link.recordConversationEventLocked(fmt.Sprintf("conversation-%d", index))
	}
	link.mu.Unlock()
	for drain := 0; drain < 2; drain++ {
		if got := link.DrainConversationEvents(); got != `[{"code":"conversation_resync","google_conversation_id":""}]` {
			t.Fatalf("drain %d = %s", drain, got)
		}
	}
	revision, ok := link.beginConversationIndexRefreshFor(generation, streamEpoch)
	if !ok || !link.replaceConversationIndexFor(generation, streamEpoch, revision, nil) {
		t.Fatal("replacement did not clear resync requirement")
	}
	if got := link.DrainConversationEvents(); got != `null` {
		t.Fatalf("post-replacement drain = %s", got)
	}
}
