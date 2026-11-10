package gmlink

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

func bootstrapObservationRequest(routeObservedAt int64) string {
	request, _ := json.Marshal(nativeRcsBootstrapRequest{
		GoogleConversationID: "synthetic-conversation", LinkGeneration: 11, SessionGeneration: 17,
		ContextRevision: 23, MappingRevision: 29, IdentityRegion: "US",
		CanonicalizerRevision: 1, CarrierBinding: nativeRcsCarrierBinding{CarrierRouteID: "android-subscription-1", AndroidSubscriptionID: 1, OwnNumberRaw: "+12025550101", OwnNumberSource: "SubscriptionManagerPhoneNumber", RouteObservedAtMS: routeObservedAt, RouteContextRevision: 23, SameHandsetEvidenceID: "fixture"},
	})
	return string(request)
}

func TestBootstrapRcsObservationRefusesAmbiguousTarget(t *testing.T) {
	link, _, cleanup, _, observedAt := nativeTargetFixture(t)
	defer cleanup()

	duplicate := cloneConversation(link.conversationRecords["synthetic-conversation"])
	duplicate.ConversationID = "synthetic-conversation-duplicate"
	link.indexConversations([]*gmproto.Conversation{duplicate})

	if got := stage2Code(t, link.BootstrapRcsObservation(bootstrapObservationRequest(observedAt))); got != "rejected" {
		t.Fatalf("code = %q", got)
	}
}

func TestBootstrapRcsObservationRefusesIncompleteAndUnsupportedSnapshots(t *testing.T) {
	for _, mutate := range []func(*Link){
		func(link *Link) { link.conversationIndexComplete = false },
		// Participants[1] is the peer; mutating self would leave the conversation eligible.
		func(link *Link) {
			link.conversationRecords["synthetic-conversation"].Participants = append(link.conversationRecords["synthetic-conversation"].Participants, link.conversationRecords["synthetic-conversation"].Participants[1])
		},
		func(link *Link) { link.conversationRecords["synthetic-conversation"].Participants[1].IsVisible = false },
	} {
		link, _, cleanup, _, observedAt := nativeTargetFixture(t)
		mutate(link)
		if got := stage2Code(t, link.BootstrapRcsObservation(bootstrapObservationRequest(observedAt))); got != "rejected" {
			t.Fatalf("code = %q", got)
		}
		cleanup()
	}
}

func TestBootstrapRcsObservationMakesNoHistoryFetch(t *testing.T) {
	link, phone, cleanup, _, observedAt := nativeTargetFixture(t)
	defer cleanup()
	fetches := 0
	link.fetchMessages = func(context.Context, string, int64, *gmproto.Cursor) (*gmproto.ListMessagesResponse, error) {
		fetches++
		return nil, errors.New("bootstrap must not fetch history")
	}
	encoded := link.BootstrapRcsObservation(bootstrapObservationRequest(observedAt))
	var result map[string]json.RawMessage
	if err := json.Unmarshal([]byte(encoded), &result); err != nil {
		t.Fatal(err)
	}
	if string(result["code"]) != `"ok"` || len(result["observation"]) == 0 {
		t.Fatalf("result = %s", encoded)
	}
	var observationJSON string
	if err := json.Unmarshal(result["observation"], &observationJSON); err != nil {
		t.Fatal(err)
	}
	var observation nativeRcsObservation
	if err := json.Unmarshal([]byte(observationJSON), &observation); err != nil {
		t.Fatal(err)
	}
	if observation.GoogleCandidateCount != 1 || observation.NativeEvidence.GoogleCandidateCount != 1 {
		t.Fatalf("candidate counts = %d, %d", observation.GoogleCandidateCount, observation.NativeEvidence.GoogleCandidateCount)
	}
	if _, ok := result["cursor"]; ok {
		t.Fatalf("cursor field present in %s", encoded)
	}
	if _, ok := result["cursorObservedAtMs"]; ok {
		t.Fatalf("cursorObservedAtMs field present in %s", encoded)
	}
	if fetches != 0 {
		t.Fatalf("history fetches = %d, want 0", fetches)
	}
	if phone.countAction("GET_CONVERSATION") != 1 {
		t.Fatal("targeted conversation was not refreshed")
	}
}
