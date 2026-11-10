package gmlink

import (
	"fmt"
	"reflect"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func groupViewFixture(t *testing.T, conversations ...*gmproto.Conversation) (*Link, groupSnapshot) {
	t.Helper()
	link := stage2LinkWithoutEventConsumer()
	stage2SeedCompleteIndex(t, link, conversations...)
	view := link.captureGroupSnapshot()
	if !view.accepted {
		t.Fatal("complete owned index was not captured")
	}
	return link, view
}

func assertGroupLookup(t *testing.T, view groupSnapshot, request []string, expected groupResultCode) groupProjection {
	t.Helper()
	result, code := lookupGroup(view, view.binding.token, request)
	if code != expected {
		t.Fatalf("lookup code=%q want=%q", code, expected)
	}
	if code != groupResolved && !reflect.DeepEqual(result, groupProjection{}) {
		t.Fatal("failure exposed group facts")
	}
	return result
}

func TestGroupProjectionBoundsAndVisibility(t *testing.T) {
	for _, size := range []int{1, 100, 101} {
		t.Run(fmt.Sprint(size), func(t *testing.T) {
			c := validGroupConversation()
			c.ConversationID = strings.Repeat("é", 128)
			c.Participants = c.Participants[1:]
			for i := 0; i < size; i++ {
				id := fmt.Sprintf("%03d", i) + strings.Repeat("x", 253)
				c.Participants = append(c.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: id}})
			}
			c.Participants = append(c.Participants, nil, &gmproto.Participant{ID: &gmproto.SmallInfo{ParticipantID: string([]byte{0xff})}, IsMe: true})
			result, code := projectGroup(c)
			if size <= 100 {
				if code != groupResolved || len(result.ParticipantIDs) != size || !result.Group {
					t.Fatal("exact local bounds or hidden filtering rejected")
				}
				_, view := groupViewFixture(t, c)
				assertGroupLookup(t, view, result.ParticipantIDs, groupResolved)
			} else if code != groupRejected || !reflect.DeepEqual(result, groupProjection{}) {
				t.Fatal("over-bound projection exposed facts")
			}
		})
	}
}

func TestGroupLookupMatchesCanonicalNumbersNotOpaqueIDs(t *testing.T) {
	conversation := validGroupConversation()
	conversation.Participants[0].ID.Number = "+15551230002"
	conversation.Participants[0].ID.ParticipantID = "opaque-first"
	conversation.Participants[1].ID.Number = "+15551230001"
	conversation.Participants = append(conversation.Participants,
		&gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "opaque-second", Number: "+15551230003"}},
		&gmproto.Participant{IsVisible: false, ID: &gmproto.SmallInfo{ParticipantID: "hidden", Number: "+15551230004"}},
	)
	_, view := groupViewFixture(t, conversation)
	result := assertGroupLookup(t, view, []string{"+15551230003", "+15551230002"}, groupResolved)
	if !reflect.DeepEqual(result.CanonicalAddresses, []string{"+15551230002", "+15551230003"}) {
		t.Fatalf("canonical addresses = %v", result.CanonicalAddresses)
	}
	assertGroupLookup(t, view, []string{"opaque-first", "opaque-second"}, groupNotFound)
}

func TestGroupProjectionRejectsMalformedVisibleFacts(t *testing.T) {
	for name, mutate := range map[string]func(*gmproto.Conversation){
		"hidden_self":         func(c *gmproto.Conversation) { c.Participants[1].IsVisible = false },
		"two_self":            func(c *gmproto.Conversation) { c.Participants = append(c.Participants, cloneGroup(c).Participants[1]) },
		"empty_self_id":       func(c *gmproto.Conversation) { c.Participants[1].ID = nil },
		"self_peer_collision": func(c *gmproto.Conversation) { c.Participants[1].ID.ParticipantID = "member-b" },
		"empty_peer_id":       func(c *gmproto.Conversation) { c.Participants[0].ID = nil },
		"invalid_peer_utf8":   func(c *gmproto.Conversation) { c.Participants[0].ID.ParticipantID = string([]byte{0xff}) },
		"invalid_self_utf8":   func(c *gmproto.Conversation) { c.Participants[1].ID.ParticipantID = string([]byte{0xff}) },
		"long_self":           func(c *gmproto.Conversation) { c.Participants[1].ID.ParticipantID = strings.Repeat("é", 129) },
	} {
		t.Run(name, func(t *testing.T) {
			c := validGroupConversation()
			mutate(c)
			result, code := projectGroup(c)
			if code != groupRejected || !reflect.DeepEqual(result, groupProjection{}) {
				t.Fatal("malformed visible facts accepted or leaked")
			}
		})
	}
}

func TestGroupProjectionReportsFactsNotSendEligibility(t *testing.T) {
	c := validGroupConversation()
	c.Type = gmproto.ConversationType_SMS
	c.ReadOnly = true
	c.Participants[1].ID.ParticipantID = strings.Repeat("é", 128)
	_, view := groupViewFixture(t, c)
	result := assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
	if !result.ReadOnly || result.Type != int32(c.Type) || result.Status != int32(c.Status) || result.SendMode != int32(c.SendMode) || !result.Group {
		t.Fatal("projection invented send eligibility or omitted actual facts")
	}
	for name, candidate := range map[string]*gmproto.Conversation{
		"nil":               nil,
		"not_group":         {ConversationID: "not-group", Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE},
		"unknown_send_mode": {ConversationID: "unknown", IsGroupChat: true, Type: gmproto.ConversationType_RCS, Status: gmproto.ConversationStatus_ACTIVE, SendMode: 99},
	} {
		t.Run(name, func(t *testing.T) {
			result, code := projectGroup(candidate)
			if code == groupResolved || !reflect.DeepEqual(result, groupProjection{}) {
				t.Fatal("unsupported projection exposed facts")
			}
		})
	}
}

func TestGroupLookupExactOpaqueSets(t *testing.T) {
	c := validGroupConversation()
	c.Participants = append(c.Participants, &gmproto.Participant{IsVisible: true, ID: &gmproto.SmallInfo{ParticipantID: "+1 (555)"}})
	_, view := groupViewFixture(t, c)
	for _, request := range [][]string{{"member-b", "+1 (555)"}, {"+1 (555)", "member-b"}} {
		original := append([]string(nil), request...)
		result := assertGroupLookup(t, view, request, groupResolved)
		if result.ConversationID != c.ConversationID || result.Binding != view.binding || !reflect.DeepEqual(result.ParticipantIDs, []string{"+1 (555)", "member-b"}) || !reflect.DeepEqual(request, original) {
			t.Fatal("lookup altered request, binding or opaque membership")
		}
	}
	for _, request := range [][]string{{"member-b"}, {"member-b", "+1 (555)", "extra"}, {"member-b", "+1555"}, {"self"}} {
		assertGroupLookup(t, view, request, groupNotFound)
	}
	for _, request := range [][]string{nil, {}, {""}, {"member-b", "member-b"}, {string([]byte{0xff})}, {strings.Repeat("x", 257)}} {
		assertGroupLookup(t, view, request, groupRejected)
	}
	oversize := make([]string, 101)
	for i := range oversize {
		oversize[i] = fmt.Sprint(i)
	}
	assertGroupLookup(t, view, oversize, groupRejected)
}

func TestGroupLookupCardinalityAndUnknownCandidates(t *testing.T) {
	_, empty := groupViewFixture(t)
	assertGroupLookup(t, empty, []string{"member-b"}, groupNotFound)
	c := validGroupConversation()
	other := cloneGroup(c)
	other.ConversationID = "other"
	_, multiple := groupViewFixture(t, c, other)
	assertGroupLookup(t, multiple, []string{"member-b"}, groupAmbiguous)
	for name, mutate := range map[string]func(*gmproto.Conversation){
		"unknown_type":            func(c *gmproto.Conversation) { c.Type = 0 },
		"unknown_status":          func(c *gmproto.Conversation) { c.Status = 99 },
		"unknown_send_mode":       func(c *gmproto.Conversation) { c.SendMode = 99 },
		"missing_self":            func(c *gmproto.Conversation) { c.Participants = c.Participants[:1] },
		"invalid_peer":            func(c *gmproto.Conversation) { c.Participants[0].ID = nil },
		"invalid_conversation_id": func(c *gmproto.Conversation) { c.ConversationID = string([]byte{0xff}) },
	} {
		t.Run(name, func(t *testing.T) {
			bad := cloneGroup(other)
			mutate(bad)
			_, view := groupViewFixture(t, c, bad)
			assertGroupLookup(t, view, []string{"member-b"}, groupUnsupported)
			assertGroupLookup(t, view, []string{"absent"}, groupUnsupported)
		})
	}
	nonGroup := cloneGroup(other)
	nonGroup.IsGroupChat = false
	_, view := groupViewFixture(t, c, nonGroup)
	assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
	assertGroupLookup(t, view, []string{"absent"}, groupNotFound)
}

func TestGroupLookupMalformedSnapshotRecordsRemainUncertain(t *testing.T) {
	for _, malformed := range []string{"nil", "key_mismatch"} {
		t.Run(malformed, func(t *testing.T) {
			link, _ := groupViewFixture(t, validGroupConversation())
			link.mu.Lock()
			if malformed == "nil" {
				link.conversationRecords["other"] = nil
			} else {
				link.conversationRecords["other"] = validGroupConversation()
			}
			link.mu.Unlock()
			view := link.captureGroupSnapshot()
			assertGroupLookup(t, view, []string{"member-b"}, groupUnsupported)
			assertGroupLookup(t, view, []string{"absent"}, groupUnsupported)
		})
	}
}

func TestGroupSnapshotRequiresAcceptedStage2State(t *testing.T) {
	for name, mutate := range map[string]func(*Link){
		"incomplete": func(l *Link) { l.conversationIndexComplete = false },
		"unowned":    func(l *Link) { l.ownerVerification = nil },
		"closed":     func(l *Link) { l.closed = true },
		"generation": func(l *Link) { l.conversationIndexGeneration++ },
		"stream":     func(l *Link) { l.conversationIndexStreamEpoch++ },
		"run":        func(l *Link) { l.runGeneration++ },
		"pending":    func(l *Link) { l.pendingConversationEvents++ },
	} {
		t.Run(name, func(t *testing.T) {
			link, _ := groupViewFixture(t, validGroupConversation())
			link.mu.Lock()
			mutate(link)
			link.mu.Unlock()
			view := link.captureGroupSnapshot()
			if view.accepted || len(view.records) != 0 {
				t.Fatal("unaccepted capture exposed records")
			}
			assertGroupLookup(t, view, []string{"member-b"}, groupStale)
		})
	}
	assertGroupLookup(t, groupSnapshot{}, []string{"member-b"}, groupStale)
}

func TestGroupSnapshotBindingAndIsolation(t *testing.T) {
	source := validGroupConversation()
	link, view := groupViewFixture(t, source)
	source.Participants[0].ID.ParticipantID = "source-mutation"
	result := assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
	result.ParticipantIDs[0] = "result-mutation"
	assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
	link.mu.Lock()
	link.conversationRecords["group-opaque"].Participants[0].ID.ParticipantID = "index-mutation"
	link.mu.Unlock()
	assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
	assertGroupLookup(t, link.captureGroupSnapshot(), []string{"index-mutation"}, groupResolved)
	link.mu.Lock()
	link.conversationRecords["group-opaque"].Participants[0].ID.ParticipantID = "member-b"
	link.mu.Unlock()
	for name, mutate := range map[string]func(*groupSnapshot){
		"generation": func(v *groupSnapshot) { v.binding.generation++ },
		"stream":     func(v *groupSnapshot) { v.binding.streamEpoch++ },
		"revision":   func(v *groupSnapshot) { v.binding.snapshotRevision++ },
		"token":      func(v *groupSnapshot) { v.binding.token = "bad" },
	} {
		t.Run(name, func(t *testing.T) {
			bad := view
			mutate(&bad)
			assertGroupLookup(t, bad, []string{"member-b"}, groupStale)
		})
	}
	if result, code := lookupGroup(view, "wrong-token", []string{"member-b"}); code != groupStale || !reflect.DeepEqual(result, groupProjection{}) {
		t.Fatal("token mismatch exposed facts")
	}
	link.mu.Lock()
	updated := cloneConversation(link.conversationRecords["group-opaque"])
	link.mu.Unlock()
	updated.Name = "name-only change"
	link.indexConversations([]*gmproto.Conversation{updated})
	newView := link.captureGroupSnapshot()
	current := assertGroupLookup(t, newView, []string{"member-b"}, groupResolved)
	if current.Binding.token == view.binding.token || !reflect.DeepEqual(current.ParticipantIDs, []string{"member-b"}) {
		t.Fatal("name change conflated snapshot and membership")
	}
	if _, code := lookupGroup(newView, view.binding.token, []string{"member-b"}); code != groupStale {
		t.Fatal("old token accepted by changed snapshot")
	}
	// Pure lookup checks its captured binding, not atomic freshness against Link.
	assertGroupLookup(t, view, []string{"member-b"}, groupResolved)
}
