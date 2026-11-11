package gmlink

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func sendReadyIndexedLink(t *testing.T, conversations ...*gmproto.Conversation) (*Link, *testHost) {
	t.Helper()
	host := &testHost{}
	link := productionVerificationLink()
	link.host = host
	link.changes = make(chan any, 4)
	link.runIncarnationID = uuid.NewString()
	link.settings = &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		RCSChats:       &gmproto.RCSChats{Enabled: true},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{}},
		SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-sim"},
	}}}
	link.settingsAt, link.settingsStreamEpoch = time.Now(), link.streamEpoch
	revision, ok := link.beginConversationIndexRefreshFor(link.generation, link.streamEpoch)
	if !ok || !link.replaceConversationIndexFor(link.generation, link.streamEpoch, revision, conversations) {
		t.Fatal("complete index was not published")
	}
	return link, host
}

func syntheticRcsChat(id, number string) *gmproto.Conversation {
	return &gmproto.Conversation{
		ConversationID: id,
		Type:           gmproto.ConversationType_RCS,
		Status:         gmproto.ConversationStatus_ACTIVE,
		Participants: []*gmproto.Participant{{
			IsVisible: true,
			ID:        &gmproto.SmallInfo{ParticipantID: id + "-peer", Number: number},
		}},
	}
}

func nativePrepareReadiness(link *Link) string {
	link.mu.Lock()
	defer link.mu.Unlock()
	return link.nativePrepareFailureReasonLocked()
}

func TestAppliedConversationEventKeepsCompleteIndexSendReady(t *testing.T) {
	link, _ := sendReadyIndexedLink(t, syntheticRcsChat("synthetic-first", "+12025550123"))
	link.mu.Lock()
	publishedRevision := link.indexRevision
	link.mu.Unlock()

	link.queueEventFor(link.generation, syntheticRcsChat("synthetic-second", "+12025550456"))
	if reason := nativePrepareReadiness(link); reason != "pending_events" {
		t.Fatalf("readiness with an unapplied conversation event = %s", reason)
	}
	link.consumeQueuedEvent(<-link.changes)

	if reason := nativePrepareReadiness(link); reason != "prepared" {
		t.Fatalf("readiness after the conversation event was applied = %s", reason)
	}
	snapshot := link.ConversationCandidateSnapshot()
	if !strings.Contains(snapshot, `"complete":true`) || !strings.Contains(snapshot, `"google_conversation_id":"synthetic-second"`) {
		t.Fatal("complete snapshot does not include the applied conversation")
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.indexRevision == publishedRevision {
		t.Fatal("applied conversation event kept the snapshot revision that earlier tickets were bound to")
	}
}

func TestConversationEventOverflowAsksHostToRescan(t *testing.T) {
	link, host := sendReadyIndexedLink(t)

	for index := 0; index <= conversationEventLimit; index++ {
		link.queueEventFor(link.generation, syntheticRcsChat("synthetic-"+uuid.NewString(), "+12025550123"))
		link.consumeQueuedEvent(<-link.changes)
	}

	if reason := nativePrepareReadiness(link); reason != "index_incomplete" {
		t.Fatalf("readiness after event correlation overflow = %s", reason)
	}
	if stale := indexStaleSignals(host); stale != 1 {
		t.Fatalf("index stale signals after overflow = %d", stale)
	}
}

func indexStaleSignals(host *testHost) int {
	host.mu.Lock()
	defer host.mu.Unlock()
	stale := 0
	for _, event := range host.events {
		if event == (eventCode{kind: "index", code: "stale"}) {
			stale++
		}
	}
	return stale
}

// eventDuringInboxScan reports a conversation update while each of the first `scans` inbox pages is in flight.
func eventDuringInboxScan(link *Link, scans int, inboxPages *int) conversationPageLoader {
	return func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
			return &gmproto.ListConversationsResponse{}, nil
		}
		*inboxPages++
		if *inboxPages <= scans {
			link.queueEventFor(link.generation, syntheticRcsChat("synthetic-updated", "+12025550456"))
			link.consumeQueuedEvent(<-link.changes)
		}
		return &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{syntheticRcsChat("synthetic-scanned", "+12025550123")}}, nil
	}
}

func TestRefreshRescansAfterConversationEventDuringScan(t *testing.T) {
	link, _ := sendReadyIndexedLink(t)
	inboxPages := 0

	code := link.refreshConversationIndexWith(context.Background(), link.generation, link.streamEpoch, eventDuringInboxScan(link, 1, &inboxPages))

	if code != "ok" || inboxPages != 2 {
		t.Fatalf("refresh code = %s after %d inbox scans", code, inboxPages)
	}
	if reason := nativePrepareReadiness(link); reason != "prepared" {
		t.Fatalf("readiness after the rescan = %s", reason)
	}
	if snapshot := link.ConversationCandidateSnapshot(); !strings.Contains(snapshot, `"google_conversation_id":"synthetic-scanned"`) {
		t.Fatal("rescan result was not published")
	}
}

func TestRefreshStopsRescanningAfterBoundedStaleScans(t *testing.T) {
	link, host := sendReadyIndexedLink(t)
	inboxPages := 0

	code := link.refreshConversationIndexWith(context.Background(), link.generation, link.streamEpoch, eventDuringInboxScan(link, conversationRefreshAttempts, &inboxPages))

	if code != "conversation_lookup_stale" || inboxPages != conversationRefreshAttempts {
		t.Fatalf("refresh code = %s after %d inbox scans", code, inboxPages)
	}
	if reason := nativePrepareReadiness(link); reason != "index_incomplete" {
		t.Fatalf("readiness after only stale scans = %s", reason)
	}
	if stale := indexStaleSignals(host); stale != 1 {
		t.Fatalf("index stale signals after only stale scans = %d", stale)
	}
}

func TestLookupIsUnknownWhileConversationEventIsUnapplied(t *testing.T) {
	link, _ := sendReadyIndexedLink(t)

	link.queueEventFor(link.generation, syntheticRcsChat("synthetic-new", "+12025550456"))
	if code := stage2Code(t, link.ResolveExistingConversation("+12025550456")); code != "conversation_lookup_unknown" {
		t.Fatalf("lookup code with an unapplied conversation event = %s", code)
	}
	link.consumeQueuedEvent(<-link.changes)

	if code := stage2Code(t, link.ResolveExistingConversation("+12025550456")); code != "conversation_found" {
		t.Fatalf("lookup code after the conversation event was applied = %s", code)
	}
}
