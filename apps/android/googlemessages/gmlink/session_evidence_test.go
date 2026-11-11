package gmlink

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// sessionEvidenceLink is a send-ready link whose run can ask the phone to resend its settings.
func sessionEvidenceLink(t *testing.T) (*Link, chan struct{}) {
	t.Helper()
	link, _ := sendReadyIndexedLink(t)
	activations := make(chan struct{}, 8)
	link.mu.Lock()
	link.runCtx = context.Background()
	link.activateSession = func(context.Context) error {
		activations <- struct{}{}
		return nil
	}
	link.mu.Unlock()
	return link, activations
}

func awaitSessionActivation(t *testing.T, activations <-chan struct{}) {
	t.Helper()
	select {
	case <-activations:
	case <-time.After(5 * time.Second):
		t.Fatal("the phone was not asked to resend its settings")
	}
}

func assertNoSessionActivation(t *testing.T, activations <-chan struct{}) {
	t.Helper()
	select {
	case <-activations:
		t.Fatal("the phone was asked to resend its settings")
	case <-time.After(50 * time.Millisecond):
	}
}

func TestReopenedStreamAsksPhoneForSettingsAgain(t *testing.T) {
	link, activations := sessionEvidenceLink(t)
	link.queueEventFor(link.generation, &events.ActiveSessionSet{})
	<-link.changes

	link.queueEventFor(link.generation, &libgm.StreamOpened{Source: libgm.StreamSource{ListenID: 2, PollAttempt: 1}})

	awaitSessionActivation(t, activations)
}

func TestFirstStreamOpenLeavesSessionActivationToConnect(t *testing.T) {
	link, activations := sessionEvidenceLink(t)

	link.queueEventFor(link.generation, &libgm.StreamOpened{Source: libgm.StreamSource{ListenID: 2, PollAttempt: 1}})

	assertNoSessionActivation(t, activations)
}

func TestPrepareWithoutSettingsAsksPhoneForSettingsAgainAtABoundedRate(t *testing.T) {
	link, activations := sessionEvidenceLink(t)
	now := time.Now()
	link.mu.Lock()
	link.sessionActivatedGeneration = link.generation
	link.settings, link.settingsAt = nil, time.Time{}
	link.ticketNow = func() time.Time { return now }
	link.mu.Unlock()
	request, _ := json.Marshal(map[string]any{
		"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now.UnixMilli()},
	})

	for attempt := 0; attempt < 2; attempt++ {
		if code := stage2Code(t, link.PrepareNativeRcs(string(request))); code != "rejected" {
			t.Fatalf("prepare without settings = %s", code)
		}
	}
	awaitSessionActivation(t, activations)
	assertNoSessionActivation(t, activations)

	now = now.Add(sessionEvidenceRequestInterval)
	link.PrepareNativeRcs(string(request))
	awaitSessionActivation(t, activations)
}

func TestRefreshAbandonsScanSupersededByNewSession(t *testing.T) {
	link, _ := sessionEvidenceLink(t)
	inboxPages := 0
	load := func(_ context.Context, request *gmproto.ListConversationsRequest) (*gmproto.ListConversationsResponse, error) {
		if request.GetFolder() != gmproto.ListConversationsRequest_INBOX {
			return &gmproto.ListConversationsResponse{}, nil
		}
		inboxPages++
		if inboxPages == 1 {
			link.queueEventFor(link.generation, &events.ActiveSessionSet{})
			<-link.changes
		}
		return &gmproto.ListConversationsResponse{
			Conversations: []*gmproto.Conversation{syntheticRcsChat("synthetic-"+uuid.NewString(), "+12025550123")},
			Cursor:        &gmproto.Cursor{LastItemTimestamp: int64(inboxPages)},
		}, nil
	}
	link.mu.Lock()
	generation, streamEpoch := link.generation, link.streamEpoch
	link.mu.Unlock()

	code := link.refreshConversationIndexWith(context.Background(), generation, streamEpoch, load)

	if code != "conversation_lookup_stale" || inboxPages != 1 {
		t.Fatalf("refresh code = %s after %d inbox pages", code, inboxPages)
	}
}
