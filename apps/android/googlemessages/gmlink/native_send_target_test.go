package gmlink

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

type nativeTargetBinder interface {
	BindNativeRcsTarget(string, string, string) string
}

func targetBinder(t *testing.T, link *Link) nativeTargetBinder {
	t.Helper()
	binder, ok := any(link).(nativeTargetBinder)
	if !ok {
		t.Fatal("native target binding is not implemented")
	}
	return binder
}

func nativeTargetFixture(t *testing.T) (*Link, *phoneFixture, func(), nativeRcsPrepareResult, int64) {
	t.Helper()
	link, phone, cleanup, _, _ := preparedRcsFixtureWithPhone(t)
	now := time.Now().UnixMilli()
	link.mu.Lock()
	link.pairingEvidenceID = uuid.NewString()
	link.settings.SIMCards[0].SIMData.InternationalPhoneNumber = "+12025550101"
	link.conversationObservedAt["synthetic-conversation"] = time.Now().Add(-time.Hour)
	link.mu.Unlock()
	request, _ := json.Marshal(map[string]any{
		"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	var prepared nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &prepared); err != nil || prepared.Code != "prepared" {
		t.Fatal("prepare failed")
	}
	decodeNativeRouteObservation(t, link.BindNativeRcsRoute(prepared.TicketID, 11, 17, 23, 29, now))
	return link, phone, cleanup, prepared, now
}

func preparedSnapshotToken(t *testing.T, prepared nativeRcsPrepareResult) string {
	t.Helper()
	var snapshot conversationCandidateSnapshot
	if err := json.Unmarshal([]byte(prepared.CandidateSnapshot), &snapshot); err != nil {
		t.Fatal(err)
	}
	return snapshot.SnapshotToken
}

func TestNativeSendBindTargetFetchesBeforeReturningFreshObservation(t *testing.T) {
	link, phone, cleanup, prepared, observedAt := nativeTargetFixture(t)
	defer cleanup()
	link.mu.Lock()
	deadline := link.nativeTicket.expiresAt
	link.mu.Unlock()
	response := targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))
	var outer struct {
		Code        string
		Observation string
	}
	if err := json.Unmarshal([]byte(response), &outer); err != nil || outer.Code != "target_bound" {
		t.Fatal("target bind failed")
	}
	var observation nativeRcsObservation
	if err := json.Unmarshal([]byte(outer.Observation), &observation); err != nil {
		t.Fatal(err)
	}
	if phone.countAction("GET_CONVERSATION") != 1 || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("target binding did not perform exactly one read-only fetch")
	}
	if observation.ObservedAtMS != observedAt || observation.NativeEvidence.ConversationObservedAtMS < observedAt || observation.NativeEvidence.ConversationObservedAtMS > time.Now().UnixMilli() {
		t.Fatal("target time is not the genuine fetch receipt time")
	}
	if observation.GoogleConversationID != "synthetic-conversation" || observation.GoogleCandidateCount != 1 || !observation.NativeEvidence.LookupComplete || !observation.DefaultOutgoing || observation.ConversationType != "Rcs" || observation.SendMode != "Automatic" || len(observation.Participants) != 2 {
		t.Fatal("full target facts were lost")
	}
	link.mu.Lock()
	if !link.nativeTicket.expiresAt.Equal(deadline) {
		t.Error("target fetch extended ticket expiry")
	}
	link.mu.Unlock()
	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "rejected" {
		t.Fatal("target bound twice")
	}
	if phone.countAction("GET_CONVERSATION") != 1 {
		t.Fatal("repeated binding fetched again")
	}
}

func TestNativeSendBindTargetRejectsChangedFetchedConversation(t *testing.T) {
	link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	phone.mu.Lock()
	changed := proto.Clone(phone.conversations[0]).(*gmproto.Conversation)
	changed.ReadOnly = true
	phone.conversations[0] = changed
	phone.mu.Unlock()
	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", preparedSnapshotToken(t, prepared))); got != "rejected" {
		t.Fatal("changed fetched proto was accepted")
	}
	if phone.countAction("GET_CONVERSATION") != 1 || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("unexpected transport calls")
	}
	link.mu.Lock()
	remaining := link.nativeTicket
	link.mu.Unlock()
	if remaining != nil {
		t.Fatal("failed fetch retained usable ticket")
	}
}

func TestNativeSendBindTargetWrongTokenRefusesBeforeNetwork(t *testing.T) {
	link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
	defer cleanup()
	if got := stage2Code(t, targetBinder(t, link).BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", "99:99:99")); got != "rejected" {
		t.Fatal("wrong snapshot accepted")
	}
	if phone.countAction("GET_CONVERSATION") != 0 || phone.countAction("SEND_MESSAGE") != 0 {
		t.Fatal("wrong snapshot reached transport")
	}
}

func TestNativeSendBindTargetRevocationAndDriftDuringFetch(t *testing.T) {
	for _, change := range []string{"revoked", "settings", "index", "runtime", "pairing", "expired"} {
		t.Run(change, func(t *testing.T) {
			link, phone, cleanup, prepared, _ := nativeTargetFixture(t)
			defer cleanup()
			defer phone.openGate()
			phone.configure(func(knobs *fixtureKnobs) {
				knobs.gateSend = true
				knobs.gateAction = gmproto.ActionType_GET_CONVERSATION
			})
			binder := targetBinder(t, link)
			token := preparedSnapshotToken(t, prepared)
			done := make(chan string, 1)
			go func() { done <- binder.BindNativeRcsTarget(prepared.TicketID, "synthetic-conversation", token) }()
			timer := time.NewTimer(2 * time.Second)
			defer timer.Stop()
		waiting:
			for {
				select {
				case action := <-phone.actionReached:
					if action == gmproto.ActionType_GET_CONVERSATION {
						break waiting
					}
				case <-timer.C:
					t.Fatal("target lookup did not reach fixture")
				}
			}
			if change == "revoked" {
				revoked := make(chan string, 1)
				go func() { revoked <- link.RevokeNativeSend(prepared.TicketID) }()
				select {
				case result := <-revoked:
					if stage2Code(t, result) != "revoked" {
						t.Fatal("lookup ticket not revoked")
					}
				case <-time.After(time.Second):
					t.Fatal("revocation waited for lookup RPC")
				}
			} else {
				link.mu.Lock()
				switch change {
				case "settings":
					link.settingsVersion++
				case "index":
					link.indexRevision++
				case "runtime":
					link.runIncarnationID = uuid.NewString()
				case "pairing":
					link.pairingEvidenceID = uuid.NewString()
				case "expired":
					deadline := link.nativeTicket.expiresAt
					link.ticketNow = func() time.Time { return deadline }
				}
				link.mu.Unlock()
			}
			phone.openGate()
			select {
			case result := <-done:
				if stage2Code(t, result) != "rejected" {
					t.Fatal("changed ticket accepted after fetch")
				}
			case <-time.After(2 * time.Second):
				t.Fatal("target lookup did not finish")
			}
			if phone.countAction("SEND_MESSAGE") != 0 {
				t.Fatal("binding dispatched a message")
			}
		})
	}
}
