package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

// probeSource is the stream every probe fixture is opened on.
var probeSource = libgm.StreamSource{ListenID: 2, PollAttempt: 1}

// probeEpoch is the fixed instant the fake clock starts from; it only moves when a test advances it.
var probeEpoch = time.Date(2026, time.November, 14, 9, 0, 0, 0, time.UTC)

// probeClock is the fake clock behind the link's ticketNow hook.
type probeClock struct {
	mu  sync.Mutex
	now time.Time
}

func (clock *probeClock) Now() time.Time {
	clock.mu.Lock()
	defer clock.mu.Unlock()
	return clock.now
}

func (clock *probeClock) Advance(delta time.Duration) {
	clock.mu.Lock()
	defer clock.mu.Unlock()
	clock.now = clock.now.Add(delta)
}

// probeShape is one list request as the loader saw it.
type probeShape struct {
	folder    gmproto.ListConversationsRequest_Folder
	count     int64
	firstPage bool
}

var (
	probeInboxShape = probeShape{folder: gmproto.ListConversationsRequest_INBOX, count: conversationPageSize, firstPage: true}
	// probeScanShapes is what one full scan requests after its inbox page.
	probeScanShapes = []probeShape{
		{folder: gmproto.ListConversationsRequest_INBOX, count: conversationPageSize, firstPage: true},
		{folder: gmproto.ListConversationsRequest_ARCHIVE, count: conversationPageSize, firstPage: true},
		{folder: gmproto.ListConversationsRequest_SPAM_BLOCKED, count: conversationPageSize, firstPage: true},
	}
)

type probeAnswer func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error)

// probeLoader is a counting conversationPageLoader. Each request is recorded before answer runs, so
// answer may inject stream events that land while the request is in flight.
type probeLoader struct {
	requests []probeShape
	answer   probeAnswer
}

func (loader *probeLoader) load(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
	call := len(loader.requests)
	loader.requests = append(loader.requests, probeShape{folder: request.GetFolder(), count: request.GetCount(), firstPage: request.GetCursor() == nil})
	if loader.answer == nil {
		return &gmproto.ListConversationsResponse{}, nil
	}
	return loader.answer(call, request)
}

// inboxAnswer serves conversations for every inbox request and empty archive and spam folders.
func inboxAnswer(conversations ...*gmproto.Conversation) probeAnswer {
	return func(_ int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
			return &gmproto.ListConversationsResponse{}, nil
		}
		return &gmproto.ListConversationsResponse{Conversations: conversations}, nil
	}
}

func assertListRequests(t *testing.T, loader *probeLoader, want ...probeShape) {
	t.Helper()
	if !reflect.DeepEqual(loader.requests, want) {
		t.Fatalf("list requests = %+v, want %+v", loader.requests, want)
	}
}

// probeLink builds a linked, running index whose complete index came from a full scan, on a fake clock.
func probeLink(t *testing.T, conversations ...*gmproto.Conversation) (*Link, *testHost, *probeClock) {
	t.Helper()
	clock := &probeClock{now: probeEpoch}
	link, host := sendReadyIndexedLink(t, conversations...)
	link.mu.Lock()
	link.ticketNow = clock.Now
	link.streamSource = probeSource
	link.conversationIndexScannedAt = probeEpoch
	link.conversationFullScanAt = probeEpoch
	link.mu.Unlock()
	return link, host, clock
}

// deliver queues one stream event as libgm would and applies everything the queue then holds.
func deliver(link *Link, event any) {
	link.mu.Lock()
	generation := link.generation
	link.mu.Unlock()
	link.queueEventFor(generation, event)
	for {
		select {
		case queued := <-link.changes:
			link.consumeQueuedEvent(queued)
		default:
			return
		}
	}
}

// retireAndReopen is the measured clean reconnect: StreamRetired, then StreamOpened two seconds later.
func retireAndReopen(link *Link, clock *probeClock) {
	deliver(link, &libgm.StreamRetired{Source: probeSource})
	clock.Advance(2 * time.Second)
	deliver(link, &libgm.StreamOpened{Source: probeSource})
}

// probeSettings is a SIM-bearing settings event with RCS enabled, as the phone sends after a session reopen.
func probeSettings() *libgm.WrappedSettings {
	return &libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{
		RCSChats:       &gmproto.RCSChats{Enabled: true},
		SIMData:        &gmproto.SIMData{SIMPayload: &gmproto.SIMPayload{SIMNumber: 1}, InternationalPhoneNumber: "+12025550101"},
		SIMParticipant: &gmproto.SIMParticipant{ID: "synthetic-outgoing"},
	}}}}
}

// ensureWith runs the revalidate-or-scan path with the counting loader, as EnsureConversationIndex does after its runtime checks.
func ensureWith(link *Link, loader *probeLoader) string {
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()
	return link.ensureConversationIndexWith(context.Background(), generation, streamEpoch, loader.load)
}

// refreshWith runs the unconditional full scan that RefreshConversationIndex performs once it has a runtime.
func refreshWith(link *Link, loader *probeLoader) string {
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()
	return link.refreshConversationIndexWith(context.Background(), generation, streamEpoch, loader.load)
}

// refreshEventCount counts the conversation_refresh diagnostics with one code.
func refreshEventCount(host *testHost, code string) int {
	host.mu.Lock()
	defer host.mu.Unlock()
	count := 0
	for _, event := range host.events {
		if event == (eventCode{kind: "conversation_refresh", code: code}) {
			count++
		}
	}
	return count
}

func requireCurrentIndex(t *testing.T, link *Link) {
	t.Helper()
	link.mu.Lock()
	defer link.mu.Unlock()
	if !link.conversationIndexCurrentLocked() {
		t.Fatal("index is not complete and current for this generation and stream epoch")
	}
}

// snapshotRecords returns the candidate records the index serves to core.
func snapshotRecords(t *testing.T, link *Link) []conversationCandidateRecord {
	t.Helper()
	var snapshot conversationCandidateSnapshot
	if err := json.Unmarshal([]byte(link.ConversationCandidateSnapshot()), &snapshot); err != nil {
		t.Fatal(err)
	}
	if !snapshot.Complete {
		t.Fatalf("snapshot is not complete: %s", snapshot.Code)
	}
	return snapshot.Conversations
}

func TestConversationProbeMatchesServedProjection(t *testing.T) {
	basis := &conversationProbeBasis{records: map[string]*gmproto.Conversation{
		"synthetic-chat": syntheticRcsChat("synthetic-chat", "+12025550123"),
	}}
	fresh := syntheticRcsChat("synthetic-chat", "+12025550123")
	fresh.Unread = true
	if !(&Link{}).conversationProbeMatches(basis, &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{fresh}}) {
		t.Fatal("non-served fields prevented probe adoption")
	}
	fresh.Name = "different served value"
	if (&Link{}).conversationProbeMatches(basis, &gmproto.ListConversationsResponse{Conversations: []*gmproto.Conversation{fresh}}) {
		t.Fatal("served field change was adopted")
	}
}

func TestSelfInducedEvidenceRotationPreservesProbeBasis(t *testing.T) {
	link, _ := sendReadyIndexedLink(t, syntheticRcsChat("synthetic-chat", "+12025550123"))
	basis := &conversationProbeBasis{records: map[string]*gmproto.Conversation{"synthetic-chat": syntheticRcsChat("synthetic-chat", "+12025550123")}, generation: link.generation, capturedAt: time.Now(), fullScanAt: time.Now(), retiredAt: true}
	link.mu.Lock()
	link.conversationProbeBasis = basis
	link.mu.Unlock()

	link.queueEventFor(link.generation, &events.ActiveSessionSet{})
	link.mu.Lock()
	activeBasis := link.conversationProbeBasis
	if activeBasis == nil || activeBasis == basis {
		link.mu.Unlock()
		t.Fatal("ActiveSessionSet did not retain or refresh the probe basis")
	}
	link.mu.Unlock()

	link.queueEventFor(link.generation, &libgm.WrappedSettings{Settings: &gmproto.Settings{SIMCards: []*gmproto.SIMCard{{}}}})
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.conversationProbeBasis != activeBasis {
		t.Fatal("settings-with-SIM dropped the retained basis without a complete live index")
	}
}

func TestConversationProbeBasisExpires(t *testing.T) {
	now := time.Now()
	link := &Link{generation: 7, ticketNow: func() time.Time { return now }}
	link.conversationProbeBasis = &conversationProbeBasis{generation: 7, capturedAt: now, fullScanAt: now.Add(-conversationFullScanMaxAge)}
	if link.conversationProbeBasisUsableLocked() {
		t.Fatal("two-hour-old full scan basis remained usable")
	}
}

func TestConversationProbeRequiresCompleteEmptyBasisForEmptyPage(t *testing.T) {
	nonempty := &conversationProbeBasis{records: map[string]*gmproto.Conversation{"synthetic-chat": syntheticRcsChat("synthetic-chat", "+12025550123")}}
	if (&Link{}).conversationProbeMatches(nonempty, &gmproto.ListConversationsResponse{}) {
		t.Fatal("empty page adopted a non-empty basis")
	}
	empty := &conversationProbeBasis{records: map[string]*gmproto.Conversation{}}
	if !(&Link{}).conversationProbeMatches(empty, &gmproto.ListConversationsResponse{}) {
		t.Fatal("empty page did not adopt an empty basis")
	}
}

func TestConversationIndexResultsDescribePublication(t *testing.T) {
	t.Run("adopted", func(t *testing.T) {
		link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
		defer cleanup()
		link.mu.Lock()
		link.conversationIndexScannedAt = time.Now().Add(-conversationIndexMaxAge)
		link.mu.Unlock()

		if result := link.EnsureConversationIndex(); result != `{"code":"ok","index":"adopted"}` {
			t.Fatalf("EnsureConversationIndex = %s", result)
		}
	})
	t.Run("scanned", func(t *testing.T) {
		link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
		defer cleanup()
		link.mu.Lock()
		link.invalidateConversationCompletenessLocked()
		link.mu.Unlock()

		if result := link.EnsureConversationIndex(); result != `{"code":"ok","index":"scanned"}` {
			t.Fatalf("EnsureConversationIndex = %s", result)
		}
	})
	t.Run("refresh", func(t *testing.T) {
		link, _, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
		defer cleanup()

		if result := link.RefreshConversationIndex(); result != `{"code":"ok","index":"scanned"}` {
			t.Fatalf("RefreshConversationIndex = %s", result)
		}
	})
}

// Spec 4.3 case 1: a clean retire and a reopen inside the gap adopt the basis with one list request.
func TestProbe01CleanReopenAdoptsWithOneRequest(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	before := snapshotRecords(t, link)
	link.mu.Lock()
	fullScanAt := link.conversationFullScanAt
	link.mu.Unlock()

	retireAndReopen(link, clock)
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	requireCurrentIndex(t, link)
	if after := snapshotRecords(t, link); !reflect.DeepEqual(after, before) {
		t.Fatalf("adopted records = %+v, want the pre-retire records %+v", after, before)
	}
	link.mu.Lock()
	unchangedFullScan := link.conversationFullScanAt.Equal(fullScanAt)
	link.mu.Unlock()
	if !unchangedFullScan {
		t.Fatal("adoption advanced fullScanAt; only a full scan may")
	}
	if refreshEventCount(host, "probe_adopted") != 1 || refreshEventCount(host, "probe_missed") != 0 {
		t.Fatal("expected one probe_adopted and no probe_missed")
	}
}

// Spec 4.3 case 2: one served field differs on page one, so the probe misses and a full scan publishes the fresh data.
func TestProbe02ServedFieldDifferenceMissesThenScans(t *testing.T) {
	cases := map[string]func(*gmproto.Conversation){
		"last message timestamp": func(c *gmproto.Conversation) { c.LastMessageTimestamp = 1_800_000_000_000_000 },
		"participant":            func(c *gmproto.Conversation) { c.Participants[0].ID.Number = "+12025550199" },
		"type":                   func(c *gmproto.Conversation) { c.Type = gmproto.ConversationType_SMS },
		"status":                 func(c *gmproto.Conversation) { c.Status = gmproto.ConversationStatus_ARCHIVED },
		"name":                   func(c *gmproto.Conversation) { c.Name = "Renamed synthetic chat" },
	}
	for name, change := range cases {
		t.Run(name, func(t *testing.T) {
			link, host, clock := probeLink(t, syntheticRcsChat("synthetic-chat", "+12025550123"))
			retireAndReopen(link, clock)
			changed := syntheticRcsChat("synthetic-chat", "+12025550123")
			change(changed)
			loader := &probeLoader{answer: inboxAnswer(changed)}
			if code := ensureWith(link, loader); code != "ok" {
				t.Fatalf("ensure = %s", code)
			}

			assertListRequests(t, loader, append([]probeShape{probeInboxShape}, probeScanShapes...)...)
			if refreshEventCount(host, "probe_missed") != 1 || refreshEventCount(host, "probe_adopted") != 0 {
				t.Fatal("expected one probe_missed and no probe_adopted")
			}
			want := []conversationCandidateRecord{conversationCandidateRecordFor("synthetic-chat", changed)}
			if records := snapshotRecords(t, link); !reflect.DeepEqual(records, want) {
				t.Fatalf("published records = %+v, want the scanned record %+v", records, want)
			}
		})
	}
}

// Spec 4.3 case 2b: fields the index does not serve may differ; the probe adopts and publishes the fresh page-one record.
func TestProbe02bNonServedDifferenceAdoptsFreshRecord(t *testing.T) {
	cases := map[string]func(*gmproto.Conversation){
		"unread":            func(c *gmproto.Conversation) { c.Unread = true },
		"latest message":    func(c *gmproto.Conversation) { c.LatestMessage = &gmproto.LatestMessage{DisplayContent: "synthetic"} },
		"latest message id": func(c *gmproto.Conversation) { c.LatestMessageID = "synthetic-message" },
		"pinned":            func(c *gmproto.Conversation) { c.Pinned = true },
		"group avatar":      func(c *gmproto.Conversation) { c.GroupAvatarURL = "https://synthetic.invalid/avatar.png" },
	}
	for name, change := range cases {
		t.Run(name, func(t *testing.T) {
			link, host, clock := probeLink(t, syntheticRcsChat("synthetic-chat", "+12025550123"))
			retireAndReopen(link, clock)
			fresh := syntheticRcsChat("synthetic-chat", "+12025550123")
			change(fresh)
			loader := &probeLoader{answer: inboxAnswer(fresh)}
			if code := ensureWith(link, loader); code != "ok" {
				t.Fatalf("ensure = %s", code)
			}

			assertListRequests(t, loader, probeInboxShape)
			if refreshEventCount(host, "probe_adopted") != 1 || refreshEventCount(host, "probe_missed") != 0 {
				t.Fatal("expected one probe_adopted and no probe_missed")
			}
			link.mu.Lock()
			published := link.conversationRecords["synthetic-chat"]
			link.mu.Unlock()
			if !proto.Equal(published, fresh) {
				t.Fatalf("published record = %v, want the fresh page-one record %v", published, fresh)
			}
		})
	}
}

// Spec 4.3 case 3: page one has an ID the basis lacks, so the probe misses and the full scan publishes it.
func TestProbe03UnknownIDMissesThenScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	added := syntheticRcsChat("synthetic-new", "+12025550199")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	loader := &probeLoader{answer: inboxAnswer(chat, added)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{probeInboxShape}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_missed") != 1 {
		t.Fatal("unknown page-one ID did not miss")
	}
	if records := snapshotRecords(t, link); len(records) != 2 {
		t.Fatalf("published %d records, want the scanned two", len(records))
	}
}

// Spec 4.3 case 4: ContinuityLost is not a clean retire, so the basis is gone and the next ensure scans.
func TestProbe04ContinuityLostScansInsteadOfProbing(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	deliver(link, &libgm.ContinuityLost{Source: probeSource, Reason: "synthetic"})
	clock.Advance(2 * time.Second)
	deliver(link, &libgm.StreamOpened{Source: probeSource})
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
		t.Fatal("continuity loss reached the probe")
	}
}

// Spec 4.3 case 5: a reopen more than conversationProbeMaxGap after the retire drops the basis.
func TestProbe05ReopenAfterGapScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	deliver(link, &libgm.StreamRetired{Source: probeSource})
	clock.Advance(conversationProbeMaxGap + time.Second)
	deliver(link, &libgm.StreamOpened{Source: probeSource})
	link.mu.Lock()
	dropped := link.conversationProbeBasis == nil
	link.mu.Unlock()
	if !dropped {
		t.Fatal("a late reopen kept the probe basis")
	}
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
		t.Fatal("late reopen reached the probe")
	}
}

// Spec 4.1 gap rule, 05b: a reopen inside the gap keeps the basis, and an ensure long after the reopen still probes.
func TestProbe05bReopenInsideGapProbesEvenWhenEnsureIsLate(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	clock.Advance(time.Minute)
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("an in-gap reopen with a late ensure did not probe")
	}
}

// Spec 4.3 case 6: a conversation event accepted after the reopen drops the basis.
func TestProbe06ConversationEventAfterReopenScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	deliver(link, syntheticRcsChat("synthetic-chat", "+12025550199"))
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
		t.Fatal("conversation event after reopen reached the probe")
	}
}

// Spec 4.3 case 7: a full scan older than conversationFullScanMaxAge is not revalidated; the scan renews it.
func TestProbe07OldFullScanScansThenProbesAgain(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	link.mu.Lock()
	link.conversationFullScanAt = probeEpoch.Add(-conversationFullScanMaxAge)
	link.mu.Unlock()
	retireAndReopen(link, clock)
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}
	assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	link.mu.Lock()
	renewed := link.conversationFullScanAt.Equal(clock.Now())
	link.mu.Unlock()
	if !renewed {
		t.Fatal("the full scan did not renew fullScanAt")
	}

	retireAndReopen(link, clock)
	again := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, again); code != "ok" {
		t.Fatalf("second ensure = %s", code)
	}
	assertListRequests(t, again, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("the reopen after a renewed scan did not probe")
	}
}

// Spec 4.3 case 8, settings half: settings with SIM on an open stream keep a basis from a complete index.
func TestProbe08aSettingsWithSIMKeepsBasisForNextEnsure(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, _ := probeLink(t, chat)
	deliver(link, probeSettings())
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("settings with SIM did not leave the next ensure a probe")
	}
}

// Spec 4.3 case 8, ActiveSessionSet half: the self-induced session rotation keeps the basis from a complete index.
func TestProbe08bActiveSessionSetKeepsBasisForNextEnsure(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, _ := probeLink(t, chat)
	deliver(link, &events.ActiveSessionSet{})
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("ActiveSessionSet did not leave the next ensure a probe")
	}
}

// Spec 4.3 case 9: an aged index is revalidated by ensure, but RefreshConversationIndex always scans.
func TestProbe09AgedIndexProbesOnEnsureButRefreshScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	t.Run("ensure probes", func(t *testing.T) {
		link, host, clock := probeLink(t, chat)
		clock.Advance(conversationIndexMaxAge)
		loader := &probeLoader{answer: inboxAnswer(chat)}
		if code := ensureWith(link, loader); code != "ok" {
			t.Fatalf("ensure = %s", code)
		}
		assertListRequests(t, loader, probeInboxShape)
		if refreshEventCount(host, "probe_adopted") != 1 {
			t.Fatal("aged index did not probe")
		}
	})
	t.Run("refresh scans", func(t *testing.T) {
		link, host, clock := probeLink(t, chat)
		clock.Advance(conversationIndexMaxAge)
		loader := &probeLoader{answer: inboxAnswer(chat)}
		if code := refreshWith(link, loader); code != "ok" {
			t.Fatalf("refresh = %s", code)
		}
		assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
		if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
			t.Fatal("refresh reached the probe")
		}
	})
}

// Spec 4.3 case 10: a basis from an earlier generation or revoked run is never used.
func TestProbe10RevokeOrNewRunDropsBasis(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	t.Run("generation mismatch is unusable", func(t *testing.T) {
		link, _, _ := probeLink(t, chat)
		link.mu.Lock()
		link.conversationProbeBasis = &conversationProbeBasis{records: map[string]*gmproto.Conversation{}, generation: link.generation, capturedAt: probeEpoch, fullScanAt: probeEpoch}
		link.generation++
		usable := link.conversationProbeBasisUsableLocked()
		link.mu.Unlock()
		if usable {
			t.Fatal("a basis from an earlier generation was usable")
		}
	})
	t.Run("revoke then a new run scans", func(t *testing.T) {
		link, host, _ := probeLink(t, chat)
		deliver(link, &libgm.StreamRetired{Source: probeSource})
		link.mu.Lock()
		captured := link.conversationProbeBasis != nil
		link.revokeLocked()
		dropped := link.conversationProbeBasis == nil
		// Run starts a new generation; the stream it opens mints the runtime capabilities.
		link.runGeneration = link.generation
		link.mu.Unlock()
		if !captured || !dropped {
			t.Fatalf("captured = %v, dropped by revoke = %v", captured, dropped)
		}
		deliver(link, &libgm.StreamOpened{Source: libgm.StreamSource{ListenID: 3, PollAttempt: 1}})
		loader := &probeLoader{answer: inboxAnswer(chat)}
		if code := ensureWith(link, loader); code != "ok" {
			t.Fatalf("ensure = %s", code)
		}
		assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
		if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
			t.Fatal("a new run reached the probe")
		}
	})
}

// Spec 4.3 case 11: a conversation event that lands during the probe request adopts nothing.
func TestProbe11ConversationEventDuringProbeAdoptsNothing(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	late := syntheticRcsChat("synthetic-late", "+12025550199")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	loader := &probeLoader{}
	loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if call == 0 {
			deliver(link, syntheticRcsChat("synthetic-late", "+12025550199"))
		}
		return inboxAnswer(chat)(call, request)
	}
	if code := ensureWith(link, loader); code != "conversation_lookup_stale" {
		t.Fatalf("superseded probe returned %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 || refreshEventCount(host, "publication_stale") != 1 {
		t.Fatal("superseded probe adopted, missed, or failed to report stale")
	}
	if snapshot := link.ConversationCandidateSnapshot(); !reflect.DeepEqual(snapshot, unavailableConversationCandidateSnapshotJSON()) {
		t.Fatalf("superseded probe published a snapshot: %s", snapshot)
	}

	again := &probeLoader{answer: inboxAnswer(chat, late)}
	if code := ensureWith(link, again); code != "ok" {
		t.Fatalf("ensure after the superseded probe = %s", code)
	}
	assertListRequests(t, again, append([]probeShape{}, probeScanShapes...)...)
	if records := snapshotRecords(t, link); len(records) != 2 {
		t.Fatalf("the full scan published %d records, want 2", len(records))
	}
}

// Spec 4.3 case 12: a probe RPC error counts as a miss and the full scan runs.
func TestProbe12ProbeErrorMissesThenScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	loader := &probeLoader{}
	loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if call == 0 {
			return nil, errors.New("synthetic list failure")
		}
		return inboxAnswer(chat)(call, request)
	}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{probeInboxShape}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_missed") != 1 || refreshEventCount(host, "probe_adopted") != 0 {
		t.Fatal("probe error was not a miss")
	}
}

// Spec 4.3 case 13: an empty page against a non-empty basis is a miss.
func TestProbe13EmptyPageAgainstNonEmptyBasisMisses(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	loader := &probeLoader{}
	loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if call == 0 {
			return &gmproto.ListConversationsResponse{}, nil
		}
		return inboxAnswer(chat)(call, request)
	}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, append([]probeShape{probeInboxShape}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_missed") != 1 {
		t.Fatal("empty page against a non-empty basis did not miss")
	}
}

// Spec 4.3 case 14: RefreshConversationIndex drops the basis on entry, so a failed scan leaves nothing to probe.
// The real refresh cannot run a scan without a runtime client, so the failed scan is run directly.
func TestProbe14RefreshDropsBasisAndFailedScanLeavesNoProbe(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	link.mu.Lock()
	captured := link.conversationProbeBasis != nil
	link.mu.Unlock()
	if !captured {
		t.Fatal("no basis captured before the refresh")
	}
	if result := link.RefreshConversationIndex(); strings.Contains(result, `"code":"ok"`) {
		t.Fatal("refresh without a runtime client reported ok")
	}
	link.mu.Lock()
	dropped := link.conversationProbeBasis == nil
	link.mu.Unlock()
	if !dropped {
		t.Fatal("RefreshConversationIndex kept the probe basis")
	}

	failing := &probeLoader{answer: func(int, *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		return nil, errors.New("synthetic list failure")
	}}
	if code := refreshWith(link, failing); code == "ok" {
		t.Fatal("failed scan reported ok")
	}
	assertListRequests(t, failing, probeInboxShape)

	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure after the failed scan = %s", code)
	}
	assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
		t.Fatal("ensure after a failed scan reached the probe")
	}
}

// Spec 4.3 case 15: an adopted index holds the old roster, the fresh fetch returns the new one, and the commit refuses.
func TestProbe15StaleAdoptedRosterRefusesAtCommit(t *testing.T) {
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	// The fixture's clock is real: the carrier route and settings are stamped from it, so the fake clock starts at now.
	clock := &probeClock{now: time.Now()}
	oldGroup := probeGroupConversation("+12025550102", "+12025550103")
	newGroup := probeGroupConversation("+12025550102", "+12025550103", "+12025550104")
	phone.mu.Lock()
	phone.conversations = []*gmproto.Conversation{oldGroup}
	phone.mu.Unlock()
	link.mu.Lock()
	link.ticketNow = clock.Now
	source := link.streamSource
	link.mu.Unlock()
	if source == (libgm.StreamSource{}) {
		t.Fatal("the fixture stream is not open")
	}
	if result := link.RefreshConversationIndex(); result != `{"code":"ok","index":"scanned"}` {
		t.Fatal(result)
	}

	// The measured reconnect: the probe adopts the index that holds the old roster.
	deliver(link, &libgm.StreamRetired{Source: source})
	deliver(link, &libgm.StreamOpened{Source: source})
	deliver(link, &events.ActiveSessionSet{})
	deliver(link, probeSettings())
	listsBefore := len(phone.listRequests())
	if result := link.EnsureConversationIndex(); result != `{"code":"ok","index":"adopted"}` {
		t.Fatalf("ensure = %s", result)
	}
	if lists := len(phone.listRequests()) - listsBefore; lists != 1 {
		t.Fatalf("adoption made %d list requests, want 1", lists)
	}
	host := link.host.(*safetyHost)
	awaitSafety(t, "probe_adopted", func() bool { return host.countEvent("conversation_refresh", "probe_adopted") == 1 })

	ticket, commit := probeGroupTicket(t, link, oldGroup, clock.Now())
	gets := phone.countAction("GET_CONVERSATION")
	phone.configure(func(knobs *fixtureKnobs) {
		knobs.getOverride, knobs.getOverrideSet = newGroup, true
	})
	if got := stage2Code(t, link.CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "members_changed" {
		t.Fatalf("commit = %s, want members_changed from the fresh fetch", got)
	}
	if phone.countAction("GET_CONVERSATION") != gets+1 || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("the commit did not fetch once and refuse before any send")
	}
	for _, record := range snapshotRecords(t, link) {
		if record.GoogleConversationID == "synthetic-conversation" && record.MemberDigest != groupMemberDigest(oldGroup) {
			t.Fatal("the adopted index does not hold the old roster")
		}
	}
}

// probeGroupConversation is a three-party RCS group; members are the visible non-self numbers.
func probeGroupConversation(members ...string) *gmproto.Conversation {
	participants := []*gmproto.Participant{{IsMe: true, IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "self", Number: "+12025550101"}}}
	for _, number := range members {
		participants = append(participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "participant-" + number, Number: number}})
	}
	return &gmproto.Conversation{ConversationID: "synthetic-conversation", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE,
		SendMode: gmproto.ConversationSendMode_SEND_MODE_AUTO, DefaultOutgoingID: "synthetic-outgoing", Participants: participants}
}

// probeGroupTicket prepares, binds, and returns a group send ticket on a link whose index is already adopted.
// It mirrors nativeGroupDispatchFixture after its index refresh. That refresh cannot be reused here: the adoption
// has to precede binding, because any later publication changes the index revision and voids the ticket.
func probeGroupTicket(t *testing.T, link *Link, group *gmproto.Conversation, now time.Time) (string, map[string]any) {
	t.Helper()
	milli := now.UnixMilli()
	link.mu.Lock()
	link.pairingEvidenceID = uuid.NewString()
	link.mu.Unlock()
	prepare, _ := json.Marshal(map[string]any{"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US", "expected_member_digest": groupMemberDigest(group), "carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": milli}})
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(prepare))), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatal("prepare failed")
	}
	if routeResult := stage2Code(t, link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, milli)); routeResult != "route_bound" {
		t.Fatalf("route bind = %s", routeResult)
	}
	if targetResult := stage2Code(t, link.BindNativeRcsTarget(prepared.TicketID, group.GetConversationID(), preparedSnapshotToken(t, prepared))); targetResult != "target_bound" {
		t.Fatalf("target bind = %s", targetResult)
	}
	facts := nativeFactsFixture(t, link)
	facts["stored_mapping"].(map[string]any)["class"] = "Group"
	encodedFacts, _ := json.Marshal(facts)
	if stage2Code(t, link.BindNativeRcsFacts(prepared.TicketID, string(encodedFacts))) != "facts_bound" {
		t.Fatal("group facts bind failed")
	}
	link.mu.Lock()
	observationJSON := link.nativeTicket.targetObservationJSON
	commandID := link.nativeTicket.request.CommandID
	digest := link.nativeTicket.request.ExpectedMemberDigest
	link.mu.Unlock()
	var observation map[string]json.RawMessage
	if err := json.Unmarshal([]byte(observationJSON), &observation); err != nil {
		t.Fatal(err)
	}
	mapping := facts["stored_mapping"].(map[string]any)
	proof := mapping["proof"].(map[string]any)
	target := map[string]any{"proof_version": 2, "peppy_conversation_id": mapping["conversation_id"], "class": "Group", "canonical_recipient": proof["canonical_participant"], "canonical_own_identity": proof["own_identity"], "evidence": observation["native_evidence"]}
	for _, field := range []string{"google_conversation_id", "conversation_type", "status", "send_mode", "read_only", "default_outgoing", "participants", "identity_region", "canonicalizer_revision", "selected_sim_id", "carrier_binding", "outgoing"} {
		target[field] = observation[field]
	}
	return prepared.TicketID, map[string]any{"command_id": commandID, "tmp_id": "opaque-group-tmp", "mapping_revision": 29, "proof_version": 2, "proof_digest": proof["digest"], "message_body": "group body", "expected_member_digest": digest, "target": target}
}

// Spec 4.3 case 16: the measured sequence end to end, then one list request adopts on the final epoch.
func TestProbe16MeasuredReopenSequenceAdoptsOnce(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	deliver(link, &libgm.StreamRetired{Source: probeSource})
	clock.Advance(2 * time.Second)
	deliver(link, &libgm.StreamOpened{Source: probeSource})
	deliver(link, &events.ActiveSessionSet{})
	deliver(link, probeSettings())
	link.mu.Lock()
	incomplete := !link.conversationIndexComplete
	link.mu.Unlock()
	if !incomplete {
		t.Fatal("the settings sequence left the index complete before the ensure")
	}
	loader := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, loader); code != "ok" {
		t.Fatalf("ensure = %s", code)
	}

	assertListRequests(t, loader, probeInboxShape)
	requireCurrentIndex(t, link)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("the measured sequence did not adopt")
	}
}

// Spec 4.3 case 17: settings land while the probe page is in flight. That call reports stale without a miss or a
// second list request, and the next ensure probes once more and adopts on the new epoch.
func TestProbe17SettingsDuringProbeReturnsStaleThenAdopts(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, host, clock := probeLink(t, chat)
	retireAndReopen(link, clock)
	loader := &probeLoader{}
	loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if call == 0 {
			deliver(link, probeSettings())
		}
		return inboxAnswer(chat)(call, request)
	}
	if code := ensureWith(link, loader); code != "conversation_lookup_stale" {
		t.Fatalf("superseded probe returned %s", code)
	}
	assertListRequests(t, loader, probeInboxShape)
	if refreshEventCount(host, "probe_missed") != 0 || refreshEventCount(host, "publication_stale") != 1 {
		t.Fatal("settings during the probe were reported as a miss or not as stale")
	}

	next := &probeLoader{answer: inboxAnswer(chat)}
	if code := ensureWith(link, next); code != "ok" {
		t.Fatalf("next ensure = %s", code)
	}
	assertListRequests(t, next, probeInboxShape)
	if refreshEventCount(host, "probe_adopted") != 1 {
		t.Fatal("the next ensure did not adopt")
	}
	requireCurrentIndex(t, link)
}

// Spec 4.3 case 18: each of these events after a clean retire drops the basis, so the next ensure is a full scan.
func TestProbe18NonRotationEventsAfterCleanRetireDropTheBasis(t *testing.T) {
	cases := map[string]any{
		"listen temporary error": &events.ListenTemporaryError{Error: errors.New("synthetic")},
		"no data received":       &events.NoDataReceived{},
		"stream gap":             &events.StreamGap{},
		"old settings":           &libgm.WrappedSettings{IsOld: true, Settings: probeSettings().Settings},
		"settings without SIM":   &libgm.WrappedSettings{Settings: &gmproto.Settings{}},
		"bare settings":          &gmproto.Settings{},
	}
	for name, event := range cases {
		t.Run(name, func(t *testing.T) {
			chat := syntheticRcsChat("synthetic-chat", "+12025550123")
			link, host, clock := probeLink(t, chat)
			retireAndReopen(link, clock)
			deliver(link, event)
			loader := &probeLoader{answer: inboxAnswer(chat)}
			if code := ensureWith(link, loader); code != "ok" {
				t.Fatalf("ensure = %s", code)
			}

			assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
			if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 0 {
				t.Fatal("the event did not drop the basis")
			}
		})
	}
}

// Review F1: a forced refresh whose scan fails must not leave a basis. The settings event below lands after
// RefreshConversationIndex has entered and before its scan begins, so it recaptures the basis from the
// still-complete index. The public refresh and the next public EnsureConversationIndex run on the phone fixture.
func TestProbeF1ForcedRefreshFailureLeavesNoBasis(t *testing.T) {
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	defer cleanup()
	clock := &probeClock{now: time.Now()}
	link.mu.Lock()
	link.ticketNow = clock.Now
	link.mu.Unlock()
	if result := link.RefreshConversationIndex(); result != `{"code":"ok","index":"scanned"}` {
		t.Fatal(result)
	}
	link.mu.Lock()
	link.conversationProbeBasis = link.captureConversationProbeBasisLocked(false)
	seeded := link.conversationProbeBasis != nil
	link.mu.Unlock()
	if !seeded {
		t.Fatal("no basis to retain")
	}

	phone.configure(func(knobs *fixtureKnobs) {
		knobs.failAction, knobs.failStatus = gmproto.ActionType_LIST_CONVERSATIONS, http.StatusInternalServerError
	})
	// Holding rpcMu makes the forced refresh wait after its entry-time clear.
	link.rpcMu.Lock()
	refreshed := make(chan string, 1)
	go func() { refreshed <- link.RefreshConversationIndex() }()
	awaitSafety(t, "the forced refresh to enter", func() bool {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.conversationProbeBasis == nil
	})
	deliver(link, probeSettings())
	link.mu.Lock()
	recaptured := link.conversationProbeBasis != nil
	link.mu.Unlock()
	link.rpcMu.Unlock()
	if !recaptured {
		t.Fatal("precondition: the settings event did not recapture a basis while the refresh waited")
	}
	if result := <-refreshed; stage2Code(t, result) != "unavailable" {
		t.Fatalf("forced refresh with failed scan = %s, want unavailable", result)
	}
	link.mu.Lock()
	leftover := link.conversationProbeBasis != nil
	link.mu.Unlock()
	if leftover {
		t.Fatal("a failed forced refresh left a probe basis for the next ensure to adopt")
	}

	phone.configure(func(knobs *fixtureKnobs) { knobs.failAction, knobs.failStatus = 0, 0 })
	before := len(phone.listRequests())
	if result := link.EnsureConversationIndex(); result != `{"code":"ok","index":"scanned"}` {
		t.Fatalf("ensure = %s", result)
	}
	shapes := phone.listRequests()[before:]
	if len(shapes) != len(probeScanShapes) {
		t.Fatalf("ensure after the failed refresh made %d list requests, want the %d of a full scan", len(shapes), len(probeScanShapes))
	}
}

// Review F1: a capture during a full scan finds no complete index, so no basis can be recreated mid-scan.
func TestProbeF1CaptureDuringScanFindsNoCompleteIndex(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, _, _ := probeLink(t, chat)
	link.mu.Lock()
	link.conversationProbeBasis = link.captureConversationProbeBasisLocked(false)
	link.mu.Unlock()
	loader := &probeLoader{}
	loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if call == 0 {
			link.mu.Lock()
			scanning := link.conversationProbeBasis == nil && !link.conversationIndexComplete
			link.mu.Unlock()
			if !scanning {
				t.Fatal("the scan began with a retained basis or a complete index")
			}
			deliver(link, probeSettings())
			link.mu.Lock()
			recaptured := link.conversationProbeBasis != nil
			link.mu.Unlock()
			if recaptured {
				t.Fatal("a capture during the scan recreated a basis")
			}
		}
		return inboxAnswer(chat)(call, request)
	}
	refreshWith(link, loader)
}

// Review F2: the full-scan ceiling also gates a recently adopted index, and only a fresh full scan renews it.
func TestProbeF2CeilingBoundsAdoptedIndexAndExpiredBasisScans(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	t.Run("zero or future full scan is never usable", func(t *testing.T) {
		link, _, _ := probeLink(t, chat)
		now := link.nowForTicketLocked()
		for name, fullScanAt := range map[string]time.Time{"zero": {}, "future": now.Add(time.Minute)} {
			link.mu.Lock()
			link.conversationFullScanAt = fullScanAt
			link.mu.Unlock()
			if link.conversationIndexUsable() {
				t.Fatalf("index with a %s full scan time was usable", name)
			}
		}
	})
	t.Run("adoption at 1h59m then ensure at 2h1m scans", func(t *testing.T) {
		link, host, clock := probeLink(t, chat)
		retireAndReopen(link, clock)
		clock.Advance(conversationFullScanMaxAge - time.Minute - 2*time.Second)
		adopt := &probeLoader{answer: inboxAnswer(chat)}
		if code := ensureWith(link, adopt); code != "ok" || refreshEventCount(host, "probe_adopted") != 1 {
			t.Fatalf("adoption at 1h59m = %s", code)
		}
		clock.Advance(2 * time.Minute)
		if link.conversationIndexUsable() {
			t.Fatal("an index whose full scan is 2h1m old was usable")
		}
		loader := &probeLoader{answer: inboxAnswer(chat)}
		if code := ensureWith(link, loader); code != "ok" {
			t.Fatalf("ensure at 2h1m = %s", code)
		}
		assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
	})
	t.Run("settings after the ceiling leave an expired basis that scans", func(t *testing.T) {
		link, host, clock := probeLink(t, chat)
		retireAndReopen(link, clock)
		clock.Advance(conversationFullScanMaxAge - time.Minute - 2*time.Second)
		if code := ensureWith(link, &probeLoader{answer: inboxAnswer(chat)}); code != "ok" {
			t.Fatalf("adoption = %s", code)
		}
		clock.Advance(2 * time.Minute)
		deliver(link, probeSettings())
		link.mu.Lock()
		retained := link.conversationProbeBasis != nil
		link.mu.Unlock()
		if !retained {
			t.Fatal("precondition: settings did not retain a basis")
		}
		loader := &probeLoader{answer: inboxAnswer(chat)}
		if code := ensureWith(link, loader); code != "ok" {
			t.Fatalf("ensure = %s", code)
		}
		assertListRequests(t, loader, append([]probeShape{}, probeScanShapes...)...)
		if refreshEventCount(host, "probe_missed") != 0 {
			t.Fatal("an expired basis was probed instead of scanned")
		}
	})
	t.Run("ceiling passing during the probe page load adopts nothing", func(t *testing.T) {
		link, host, clock := probeLink(t, chat)
		retireAndReopen(link, clock)
		clock.Advance(conversationFullScanMaxAge - time.Minute - 2*time.Second)
		loader := &probeLoader{}
		loader.answer = func(call int, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
			if call == 0 {
				clock.Advance(2 * time.Minute)
			}
			return inboxAnswer(chat)(call, request)
		}
		if code := ensureWith(link, loader); code != "ok" {
			t.Fatalf("ensure = %s", code)
		}
		assertListRequests(t, loader, append([]probeShape{probeInboxShape}, probeScanShapes...)...)
		if refreshEventCount(host, "probe_adopted") != 0 || refreshEventCount(host, "probe_missed") != 1 {
			t.Fatal("a page that crossed the ceiling was adopted, or was not reported as a miss")
		}
	})
}

// Review F3: a full scan that publishes sets fullScanAt and drops the prior basis in the same critical section.
func TestProbeF3ScanPublicationIsAtomicWithBasisAndFullScanTime(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	link, _, clock := probeLink(t, chat)
	link.mu.Lock()
	link.conversationProbeBasis = link.captureConversationProbeBasisLocked(false)
	link.mu.Unlock()
	clock.Advance(time.Hour)
	revision, started := link.beginConversationIndexRefreshFor(link.generation, link.streamEpoch)
	if !started {
		t.Fatal("scan did not begin")
	}
	if !link.replaceConversationIndexForContext(context.Background(), link.generation, link.streamEpoch, revision, []*gmproto.Conversation{chat}) {
		t.Fatal("scan publication was refused")
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.conversationProbeBasis != nil || !link.conversationFullScanAt.Equal(clock.Now()) {
		t.Fatalf("publication left basis %v and fullScanAt %v, want no basis and fullScanAt %v", link.conversationProbeBasis != nil, link.conversationFullScanAt, clock.Now())
	}
}

// Review F3: a clean retire or settings event right after a successful scan leaves a usable basis, so the next reopen probes.
func TestProbeF3EventAfterScanPublicationLeavesUsableBasis(t *testing.T) {
	chat := syntheticRcsChat("synthetic-chat", "+12025550123")
	// A clean reopen after the scan probes on its own reopen; a settings event after the scan keeps a fresh
	// basis for the next ensure. Both must find the basis that the scan's publication left in place.
	triggers := map[string]func(*probeClock, *Link){
		"clean reopen": func(clock *probeClock, link *Link) { retireAndReopen(link, clock) },
		"settings":     func(_ *probeClock, link *Link) { deliver(link, probeSettings()) },
	}
	for name, trigger := range triggers {
		t.Run(name, func(t *testing.T) {
			link, host, clock := probeLink(t, chat)
			if code := refreshWith(link, &probeLoader{answer: inboxAnswer(chat)}); code != "ok" {
				t.Fatalf("scan = %s", code)
			}
			trigger(clock, link)
			loader := &probeLoader{answer: inboxAnswer(chat)}
			if code := ensureWith(link, loader); code != "ok" {
				t.Fatalf("ensure = %s", code)
			}
			assertListRequests(t, loader, probeInboxShape)
			if refreshEventCount(host, "probe_adopted") != 1 {
				t.Fatal("the reopen after a fresh scan did not probe")
			}
		})
	}
}
